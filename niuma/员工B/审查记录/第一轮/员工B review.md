> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 第 1 轮审查：Engine 连续 Chat 回合

日期：2026-09-23  
审查人：项目负责人  
结论：**CHANGES_REQUIRED，返工后提交第 1.1 轮。**

报告：[员工 B 第 1 轮报告](<../../提交报告/第一轮/员工B（Engine连续Chat回合 第1轮报告）.md>)  
依据：[员工 B 任务单](../../任务/第一轮/员工B提示词.md)、[项目开发守则](../../../../项目开发守则.md)  
证据：[复现附件与说明](员工B审查附件/README.md)

## 1. 结论与边界

连续三轮、对象身份保留、通常情况下的幂等回放、事务回滚等主路径已有实现；负责人复跑原有测试得到 **83 通过、0 失败、1 个付费 live 忽略**，fmt、Clippy、WASM 检查均退出 0。

仍不能放行：最新回合查询实际取最后写入事件，历史回合可以因此重开；追加事务没有确认候选消息使用的是当前前序快照，合法 resume 后可能丢失刚完成的对话。此外，畸形上下文、多 Agent 和错误 workspace 未按任务要求拒绝，若干验收测试没有到达其声称验证的分支。

本轮复核新增 6 个审查探针，均在隔离副本执行。其中 **5 个行为反例对应下列 B-R1～B-R4；另 1 个是 B-C1 的错误诊断建议**。不能把第 6 项混写成同等严重的业务漏洞。探针失败表示当前实现未满足探针的期望断言，不表示原有 83 项测试失败。

- 本次只新增审查文档、附件与隔离复现；没有代改 B 的正式实现或测试。
- C 正在开发。没有修改其 `server.rs`、`tests/http_contract.rs` 或运行实例；审查快照之后 C 的 `server.rs` 已继续变化，当前结果不代表 C 已通过验收。
- B 新命令暂不冻结为已验收接口；C 继续现有 HTTP/SSE 任务，新增 Turn 写路由仍等待 B 验收。
- 不要求推倒 A 的基础、升级 schema、引入 Scheduler 或重写 Provider。问题可以在 B 现有文件边界内修复。

## 2. 审查基线与实际验证

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`。审查对象是包含 A 最终修复和 B 交付的未提交工作区，不能用 HEAD 单独替代。

为避开 C 并行修改，复制 Rust 源码、测试、清单、锁文件及构建脚本至：

`./.local/temp/fufu-b-review-50496aff95`

只复用 `./NoManCode\rust-app\target` 编译缓存；测试数据库由临时夹具创建，Provider 是本机 mock。没有复制生产数据库或调用付费模型。B 的四个实现文件、`session_turns.rs` 及两份兼容测试在最终复核时与快照哈希一致，详见附件 `source-recheck.json`。

| 检查 | 实际结果 | 证据/说明 |
| --- | --- | --- |
| 原有 workspace 测试 | 83 通过、1 忽略；退出 0 | `review-baseline-test.log`；43 lib、4 adversarial、9 final_acceptance、10 runtime、12 session_turns、5 protocol |
| `build.ps1 -Action fmt` | 退出 0，无输出 | `review-gates.json`；只检查，不自动修改 |
| `build.ps1 -Action clippy` | 退出 0 | `review-clippy.log`；workspace/all-targets、`-D warnings` |
| `build.ps1 -Action wasm-check` | 退出 0 | `review-wasm-check.log` |
| 审查探针 `review_` | 0 通过、6 失败、12 过滤；退出 101 | `review-probes.log`；含 5 条业务反例和 1 条诊断建议 |

原有检查在不包含审查探针的源码上执行；探针没有加入共享 `NoManCode/rust-app/tests`。`proc-macro-error2` 的 future-incompat 提示仍是警告，不是本轮检查失败。

## 3. 必须修复的问题

### B-R1 · P1：最新回合被旧轮次的后续事件改变，历史保护失效

**入口：** [repository.rs:858](../../../../NoManCode/rust-app/src/repository.rs#L858)、[store.rs:792](../../../../NoManCode/rust-app/src/store.rs#L792)。对应 B-05、B-07。

报告和注释写的是“各 Turn 的首条事件 seq”；实际 SQL 是将所有事件按 `e.seq DESC` 排序取第一条。这两个算法不同。

有效复现：T1 完成 → 追加 T2 并完成 → 通过既有 `Store::event` 给 T1 再写一条 completed 状态事件 → 查询 latest → 调用 `Store::resume_task(T1)`。实际得到：

```text
latest_is_old=true, historical_resume_accepted=true
```

T1 的补充事件把 T2 挤出了“最新”位置，已存在后续轮次的 T1 被重新排队。原测试没有覆盖这一点。

**具体修法：** 在事务内先确认每个 Turn 都有有效所属事件，再按“各 Turn 最小 seq”的最大值选择最新回合。例如：

```sql
SELECT t.id, MIN(e.seq) AS opening_seq
FROM turns t
LEFT JOIN events e
  ON e.turn_id = t.id AND e.session_id = t.session_id
WHERE t.session_id = ?1
GROUP BY t.id
ORDER BY opening_seq DESC;
```

必须先检查所有行的 `opening_seq` 非 NULL；不能仅取第一行而忽略其他损坏轮次。可以保留现有缺失检查，再把最终查询改为按分组后的 `MIN(seq)` 排序。追加和 resume 使用同一查询；保持 schema 6。注意该算法以创建事件按现有追加规则保留为前提：非 NULL 检查能发现完全没有所属事件，不能单独证明创建事件从未被删除。若报告要声称识别“仅删除创建事件、仍保留后续事件”的损坏，必须另给可验证的 opening 事件识别规则，不能声称 MIN 查询已经证明。

**闭合验收：** 建立两个真实存在、同秒创建且 ID 字典序与提交顺序相反的 Turn；向 T1 补事件后 latest 仍为 T2，T1 追加及 resume 都被拒绝，T1 内容/状态/事件不因被拒绝的操作而改变，T2 完成后仍能正常追加。某 Turn 缺少任何可用于判序的所属事件时继续明确拒绝。

### B-R2 · P1：前序 ID 未变不等于前序内容未变，旧候选快照会漏掉合法 resume

**入口：** [engine.rs:431](../../../../NoManCode/rust-app/src/engine.rs#L431)、[store.rs:632](../../../../NoManCode/rust-app/src/store.rs#L632)。对应 B-03、B-06、B-07。

Engine 在写事务外读取并复制 `previous.messages`。Store 虽在 `BEGIN IMMEDIATE` 后重查前序状态，却只确认 ID、completed 和工具权限，没有确认待插入 messages/route/workspace 来自当前持久化前序。

复现按真实操作构造交错：读取已完成 T1 并准备 T2 候选 → 使用真实 `Engine::resume(T1, "intervening-resume-message")` 并等待完成 → 使用独立 `Store::open` 提交原候选。T1 仍是同一个已完成 Turn，当前事务允许提交；新 T2 却没有中间这次对话：

```text
stale_snapshot_accepted=true, intervening_history_preserved=false
```

这不是要求新增分支功能，也不是人为修改旧消息。它是本轮允许的最新轮次 resume 与追加之间的读写竞争。一个 Engine 的内存 gate 不能替代跨连接存储保证。

**具体修法，选一个完整落地：**

1. 推荐最小改动：Engine 将构建候选所用的前序语义快照或其稳定摘要传给 Store。Store 在立即写事务中、回放检查之后，重读当前前序并比较；消息、route、workspace、spec 及身份关联必须一致。不同则返回可分类的前序变化冲突，整笔不提交。不要只比较 `updated_at`，它是秒精度。摘要不含凭据，也不落日志。
2. 也可让 Store 在立即写事务中根据 typed command 和当前持久化前序构造下一轮消息与任务。此方案仍要保证 Engine 的 route/key 预检与实际提交的 route 一致；不能事务外检查旧 route，事务内静默切到另一 route。不要在 SQLite 事务内等待网络调用。

顺序保持：幂等分类/回放 → 当前归属与最新前序检查 → 当前快照检查与上下文限制 → 原子写入 → commit → 仅新建方 launch。同 key 已提交结果仍应先回放，不能因为后续状态变化而拒绝原结果。

**闭合验收：** 用同步屏障或明确的候选构造/提交阶段稳定重现上述交错；结果只能是冲突且零新增，或者提交包含刚完成 resume 的完整消息。另测追加先提交时旧轮次 resume 拒绝；resume 正在运行时追加拒绝。检查原有快照、Turn/Task/幂等/事件数量与 Provider 调用次数，不靠随机 sleep 碰时序。

### B-R3 · P2：消息校验把错误类型的 tool_calls 当成“没有工具调用”

**入口：** [engine.rs:22](../../../../NoManCode/rust-app/src/engine.rs#L22)，尤其第 36 行。对应 B-02、B-03。

`as_array()` 返回 None 同时可能表示字段不存在、null 或类型错误。当前分支直接跳过；空消息列表也会通过，且未校验基本 content 形状。

在临时数据库中将前序 messages 故障注入为：

```json
[{"role":"assistant","tool_calls":{"id":"bad"}}]
```

再提交短合法 user message，实际新建成功且 mock Provider 多调用一次：

```text
malformed_prefix_accepted=true, provider_call_delta=1
```

**具体修法：** 先定义本轮现有 Provider 会生成的合法消息形状；区分字段缺失、允许的空值和非法类型。非空历史、role/content、工具调用结构和调用/结果配对要明确校验。对本轮不支持的历史明确返回 `UnsupportedSession`，不能通过删除字段或裁剪消息“修好”历史；不要顺便扩大成任意多模态/工具会话支持。

同时结合 B-R2，保证提交的是已验证的那份快照。若 Store 可直接接受候选，事务入口不能接受与当前前序不一致的消息。

**闭合验收：** 保留合法纯文本前缀成功用例，分别覆盖空前缀、错误类型的 tool_calls、缺失必要字段/非法 content、未配对工具消息。每个失败用例都用合法 Session/前序/key，确认零新增、没有 launch、Provider 调用不增加。

### B-R4 · P2：未落实单 Agent 与 workspace 归属拒绝

**入口：** [engine.rs:410](../../../../NoManCode/rust-app/src/engine.rs#L410)、[engine.rs:449](../../../../NoManCode/rust-app/src/engine.rs#L449)、[store.rs:628](../../../../NoManCode/rust-app/src/store.rs#L628)。对应 B-02、B-03、B-06。

两项独立复现：

- 用公开 `Store::insert_agent` 为原 Session 增加第二个 Agent，继续用原 Agent 追加。实际成功，Turn 从 1 增至 2。当前代码只检查所选 Agent 属于 Session，没有确认 Session 只有一个 Agent。
- 在临时库把前序 Task.workspace 改为其他项目路径，提交合法短消息。实际成功，代码将新 Task.workspace 直接设为 Project.root_path，掩盖了损坏的前序归属。

**具体修法：** 在 Store 同一写事务中查询并确认 Session 恰有一个 Agent，且就是命令指定 Agent；保留前序唯一 Task 与依赖检查。对前序 workspace 使用项目已有路径规范核对 Project.root_path，确认匹配后复用已验证的快照。不要引入一套会误判 Windows 合法等价路径的临时字符串规则，也不要静默改写来自错误项目的快照。Engine 可预检，但 Store 必须是最终裁决。

**闭合验收：** 多 Agent、真实存在但属于其他 Session 的 Agent、前序 workspace 不属于 Project 均拒绝且没有持久化副作用/launch。保留“只改全局 Settings.workspace，合法旧项目仍可继续”的成功用例，防止把两个场景混淆。

### B-R5 · P2：验收证据与所声称的场景不一致

**入口：** [session_turns.rs:554](../../../../NoManCode/rust-app/tests/session_turns.rs#L554)、[session_turns.rs:705](../../../../NoManCode/rust-app/tests/session_turns.rs#L705)、[session_turns.rs:783](../../../../NoManCode/rust-app/tests/session_turns.rs#L783)、[session_turns.rs:879](../../../../NoManCode/rust-app/tests/session_turns.rs#L879)。

| 条目 | 当前实际测到什么 | 返工方式 |
| --- | --- | --- |
| T6 同秒逆序 | 只有一个旧 Turn 被重命名；所谓 stale ID 根本不存在，命中 NotFound | 创建两个真实 Turn，固定同秒且 ID 逆序；旧 ID 应命中 StalePredecessor，并加 B-R1 旧事件反例 |
| T9 重启/草稿 | 对同一对象调用 recover；把已取消行手工改为 running+partial，未关闭后重开，也未验证 commit 后未 launch | 拆成三个独立夹具：已持久取消重开后仍取消；真实 mock 流产生部分输出后停止旧运行并重开 Store/Engine；只提交 queued 而不 launch 后重开。均断言恢复/回放不调用 Provider、草稿和身份保留 |
| T10 竞争 | T2 完成后顺序调用 T1 resume，未触发读快照至提交之间的交错 | 增加 B-R2 确定性时序，覆盖竞争两种方向与 resume 运行中 |
| T11 上下文上限 | `"字".repeat(1_500_001)` 本身已超过单条 100,000 字节限制，未测累计上下文限制；所用前序还已变旧 | 使用当前真实 completed 前序、有效路由和短新消息；构造格式合法的累计历史超过 1,500,000 字节，另测限内成功。断言明确错误和零新增调用 |
| T3/T5 并发 | 有独立连接和线程，但缺乏开始屏障；Store 竞争不能单独证明 Engine 只派发一次 | 增加 barrier/通知控制竞争阶段，并补应用命令并发重试的 mock 调用次数，保留跨 Store 场景 |
| T4/T7 身份与损坏 | T4 未覆盖已有的不同 Session/Agent；T7 的错误 Agent 不存在，未覆盖真实错误归属/多 Agent | 分别准备真实对象；复用 B-R3/B-R4 反例，避免因不存在或外键错误提前失败 |
| T8 调用次数 | 失败后立即读计数，当前线程 Tokio 中错误 spawn 可能尚未被调度 | 加 `!engine.is_busy()` 等同步可观测断言，并让执行队列有机会推进；断言无残留活动任务和无 Provider 请求 |

route 删除未单独测试，报告已经如实披露；第 1.1 轮同时补齐原 T11 的删除场景即可，不能把它写成已发现的凭据泄漏。

这些不是要求把每个测试写成大型框架。使用现有 mock、临时库和少量同步控制即可。保留有价值的原测试，修正错误夹具，不删断言、不降低任务要求来追求通过数量。

## 4. 非阻断建议及审查方更正

### B-C1：区分不存在、损坏和存储错误，保留底层原因

[engine.rs:404](../../../../NoManCode/rust-app/src/engine.rs#L404) 等入口用 `.map_err(|_| ChatTurnError::NotFound)` 丢弃所有底层错误。临时库将已有 Agent 的 `created_at` 设为 -1，使行解码失败；实际被报告为 `NotFound`。探针的“损坏不应叫 NotFound”断言因此失败，但请求确实已被拒绝。

**范围判定：** 当前 [domain.rs:523](../../../../NoManCode/rust-app/src/domain.rs#L523) 明确将 missing 或 corrupt 合并为 NotFound。审查不能反过来把这个探针说成已违反现有枚举定义，也不能声称坏数据被放行。该项是诊断与后续 HTTP 映射改进，不是独立的本轮强阻断。

负责人建议：只把确定的不存在映射为 NotFound；其他 SQLite/解码错误保留原因链。需要时可在 B 所有的 domain 中增设 `CorruptState` 等应用错误；缺少首事件一并归类。该方向已允许在 B 范围内处理，无须用户重复批准；本轮不修改 protocol 或 C 的 HTTP 映射。即使暂缓，也要在报告里注明边界，后续写接口接入前再统一，不能直接把所有内部故障暴露为 404。

审查复现实验也做过夹具更正：最初尝试直接 DROP agents 被外键提前拒绝，不能证明错误分类问题，已弃用；最终保存的探针使用有效的行解码故障，日志来自更正后的完整重跑。这符合守则“审查方也验证自己的反例”的要求。

### B-C2：环境表述应区分 PATH 与机器安装情况

B 报告写“本机没有 PowerShell 7”，又说明使用临时目录 shim。本次负责人实际使用的是 PowerShell **7.6.5**：

`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`

负责人已在该真实宿主下完成本轮门禁，因此不因 B 的 shell 环境描述继续阻塞。B 第 1.1 轮应记录自身 `Get-Command pwsh` 的路径和 `$PSVersionTable`，把事实表述为当时进程 PATH 的解析结果；不要由 PATH 缺失推断整台机器未安装，也不要将 Windows PowerShell shim 的结果算作 PS7 验证。

## 5. 第 1.1 轮返工顺序与放行标准

1. 先修 B-R1，使 append 与历史 resume 的顺序判断一致。
2. 修 B-R2，把候选来源与事务当前快照绑定；明确冲突类型和回放顺序。
3. 修 B-R3/B-R4，将有效上下文、单 Agent、workspace 归属纳入可信提交边界。
4. 按 B-R5 校正 T1～T12 证据；将业务反例整理为 B 正式测试。审查附件是反例起点，不能机械复制全部诊断建议为已有硬性需求。
5. 运行原任务要求的 test/fmt/clippy/wasm-check 和 diff 检查，保持 A 的回归；用真实命令、退出码、忽略项和环境记录结果。C 正在改动导致的共享检查问题要单独说明，不覆盖其文件。

放行要求：B-R1～B-R5 逐项有实现位置、能命中目标分支的测试和副作用断言；基础回归通过；报告不再把未验证场景标为“已验证”。B-C1 是否本轮改善如实写明，不能让它掩盖两处 P1。

仍沿用 B 原文件所有权与 schema 6，不接 HTTP、不改 C 文件、不全仓自动格式化、不自行提交包含别人员工修改的整包 diff。

## 6. 可直接发给员工 B 的返工提示词

```text
你是员工 B，继续 Engine 连续 Chat 回合任务。工作目录 ./。

先完整阅读：
./niuma\员工B\审查记录\第一轮\员工B review.md
./niuma\员工B\审查记录\第一轮\员工B审查附件\README.md
并沿用原任务单、项目开发守则和 A 已验收基线。

本轮结论 CHANGES_REQUIRED。直接实现并测试 B-R1～B-R5：
1. 用每个 Turn 的首事件 MIN(seq) 判定提交顺序，防止旧事件重开历史。
2. 在追加写事务中确认前序语义快照，解决合法 resume 完成后旧候选漏历史。
3. 拒绝畸形上下文、多 Agent 和错误 workspace 归属，失败零新增、零派发。
4. 校正 T6/T9/T10/T11 等测试夹具与并发时序，补齐原任务要求的证据。

Review 已给具体修改入口、SQL/事务思路、反例与闭合标准。不要只改报告或测试数量。
B-C1 是诊断建议，按 review 的范围判定处理并如实报告；B-C2 修正环境描述。

C 正在并行工作：不要修改 server.rs、tests/http_contract.rs 或 C 文档；
不要接新增 HTTP 写路由，不升级 schema，不覆盖 A 已验收改动，不全仓自动格式化。
在你拥有的实现和测试中完成修复，运行门禁。不要自行宣布 review 通过。

新报告写到：
./niuma\员工B\提交报告\第一轮\员工B（Engine连续Chat回合 第1.1轮报告）.md

报告逐项保留 B-R1～B-R5 编号，列出代码位置、实际测试、失败副作用断言、
命令和退出码、尚未完成项。保留第一轮报告，不覆盖历史。
```
