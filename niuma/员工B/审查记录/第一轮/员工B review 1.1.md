> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 第 1.1 轮复审：Engine 连续 Chat 回合

日期：2026-09-23  
审查人：项目负责人  
结论：**CHANGES_REQUIRED。上轮两处 P1 已修复；B-R3 与 B-R5 仍需完成，进入第 1.2 轮。**

依据：[B 第 1.1 轮报告](<../../提交报告/第一轮/员工B（Engine连续Chat回合 第1.1轮报告）.md>)、[上一轮 review](<员工B review.md>)、[B 原任务](../../任务/第一轮/员工B提示词.md)、[项目开发守则](../../../../项目开发守则.md)。  
证据：[第 1.1 轮复审附件](员工B第1.1轮审查附件/README.md)。

## 1. 本轮结论

本轮有实质进展。负责人复跑 **94 项通过、0 失败、1 个付费 live 忽略**，fmt、Clippy、WASM 检查均退出 0；C 审查期间观察到的 B 中间态 T3 同步等待阻塞已不再出现。上轮 6 个审查探针全部通过，其中旧候选冲突探针本次还明确断言了 `PredecessorChanged`。

仍不放行的核心是：**畸形工具调用结构尚能进入 Provider；若干测试仍没有建立其声称的前置条件。** 本次沿用原问题编号，不把已经修好的 P1 继续列作未修，也不因原测试全绿忽略剩余反例。

| 编号 | 复审状态 | 依据 |
| --- | --- | --- |
| B-R1 · 原 P1 | 实现闭合 | 每轮 `MIN(seq)` 排序；所有 Turn 都检查事件是否缺失；旧轮补事件不改变 latest，不再重开历史 |
| B-R2 · 原 P1 | 原反例闭合 | 写事务内比较真实前序快照；合法 resume 完成后旧候选返回 PredecessorChanged；先回放再检查资格 |
| B-R3 · P2 | 部分修复，未闭合 | object 型 tool_calls 已拒绝；但配对的调用若 `function:{}` 仍被接受并调用 Provider |
| B-R4 · P2 | 实现闭合 | Store 内单 Agent 计数、归属、规范化 workspace 校验；Engine 复用原 workspace；独立错误路径反例拒绝 |
| B-R5 · P2 | 部分修复，未闭合 | 上下文累计上限、T8 调度断言等已改善；真正部分流重启、并发、逆序与单故障夹具仍不足 |
| B-C1 · 诊断建议 | 部分完成 | Agent 负时间已是 CorruptState；Session/Turn 解码失败仍可能是 NotFound |
| B-C2 · 环境说明 | 已纠正 | 报告区分 PS7 宿主与旧 shim；本次复核直接使用真实 PS7 |

B 新命令仍未整体验收冻结，不接 HTTP 写路由。C 按自己的 review 独立返工。本轮负责人未修改共享实现或正式测试，未提交、发布或替换运行实例。

## 2. 固定基线与实测结果

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`。实际基线是含 A 修复和 B 交付的未提交源码；复审副本为：

`./.local/temp/fufu-b11-review-30b643772f`

使用此前由负责人创建、当前无测试进程占用的隔离编译缓存 `./.local/temp/fufu-c-review-22cad04b86\target`，没有使用员工正在构建的共享 target。源码来自 B 1.1 副本，不是该缓存目录原先的 C 源码。

审查结束时 B 的四个实现文件、`session_turns.rs` 和兼容测试与副本哈希一致。C 的 server/HTTP 测试已继续变化，因此本次 94 项通过仅代表固定快照，不能当作 C 最新返工验收。详见附件 manifest 和 source-recheck。

| 检查 | 实际结果 | 证据 |
| --- | --- | --- |
| `build.ps1 -Action test` | 94 通过、1 忽略，退出 0 | `review-test.log`；43 lib、4 adversarial、9 final_acceptance、11 http_contract、10 runtime、12 session_turns、5 protocol |
| `build.ps1 -Action fmt` | 退出 0，无输出 | `review-gates.json`；只检查 |
| `build.ps1 -Action clippy` | 退出 0 | `review-clippy.log`，workspace/all-targets、`-D warnings` |
| `build.ps1 -Action wasm-check` | 退出 0 | `review-wasm-check.log` |
| 9 项审查探针 | 6 通过、3 失败，退出 101 | `review-probes.log`，原有 12 项夹具测试被过滤 |

3 项失败分别是 **B-R3 的实现反例、B-R5 的测试前置条件反例、B-C1 的诊断反例**，不能写成“3 处 P1”或“员工原有 94 项失败”。`proc-macro-error2` future-incompat 仍是警告。

运行宿主为 PowerShell 7.6.5，`Get-Command pwsh` 指向：

`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`

本次未加入 Windows PowerShell shim。所有 Provider 为本机 mock，测试凭据为合成值，未执行付费 live。

## 3. 已闭合的问题，不再要求重做

### B-R1：提交顺序和历史保护

[repository.rs:916](../../../../NoManCode/rust-app/src/repository.rs#L916) 使用分组后的最小 seq，并遍历拒绝任一没有所属事件的 Turn。负责人旧事件反例现在得到：

```text
latest_is_old=false, historical_resume_accepted=false
```

“创建事件单独被删除、后续事件仍存在”无法仅由 MIN 查询识别，报告已如实限定，不因此追加 schema 要求。T6 的逆序夹具问题仍列入 B-R5，但不否认实现已改对。

### B-R2：前序快照竞争

[store.rs:664](../../../../NoManCode/rust-app/src/store.rs#L664)、[store.rs:824](../../../../NoManCode/rust-app/src/store.rs#L824) 在立即事务内比较 messages 前缀、route、workspace 和复制的 spec 字段，不依赖秒精度时间戳。旧候选在真实 resume 完成后提交，负责人探针明确得到 `PredecessorChanged`。回放位于比较之前，保持已有请求回放语义。

继续保留该实现与有效反例，下一轮只补真实 resume 运行中等剩余证据，不重写整套事务。

### B-R4：单 Agent 与项目归属

[store.rs:643](../../../../NoManCode/rust-app/src/store.rs#L643)、[store.rs:660](../../../../NoManCode/rust-app/src/store.rs#L660) 已将 Agent 数量和路径检查放入同一写事务。负责人使用独立合法夹具分别增加第二 Agent、改错前序 workspace，两种请求都被拒绝。当前 Engine 也复用已验证的前序路径。

原测试把两个故障叠在一起的问题仍需修正，以便未来回归确实能发现守卫失效；不将其混说成当前实现仍放行。

## 4. B-R3 · P2：配对并不能使不完整的工具调用变合法

**入口：** [engine.rs:35](../../../../NoManCode/rust-app/src/engine.rs#L35) 的 `tool_call_ids`、[engine.rs:55](../../../../NoManCode/rust-app/src/engine.rs#L55) 的前缀校验。

现在校验要求 `function` 为对象，却不要求函数名和参数。负责人从正常完成、含 user 和非空 assistant content 的前缀出发，仅加入如下调用和配对结果，再提交合法短消息：

```json
{"id":"incomplete-call","type":"function","function":{}}
```

配对的 tool 消息有相同 ID、非空 content，之后还有合法 assistant 消息，因此不会被“缺 user”“无 content”“未配对”提前挡住。实际结果：

```text
incomplete_function_accepted=true, provider_call_delta=1, turn_delta=1
```

这是上轮“畸形上下文必须在新增和调用 Provider 前拒绝”的未闭合边界，不是要求本轮实现新工具能力。

### 本轮明确采用的最小修复方向

本切片限定无工具 Chat，负责人建议并批准收窄为其真实支持的历史：

1. 保留正常纯文本 system/user/assistant 前缀与必要 content 校验。
2. 明确拒绝 `role=tool`、非空 tool_calls 和非法类型的 tool_calls；如保留空数组的兼容性，在规则与测试中明确，不把它等同于允许任意工具历史。
3. 错误返回 `UnsupportedSession`，不删字段、不裁剪、不生成伪结果，不顺手实现完整工具消息协议。
4. 保留 B-R2 对被验证快照与提交快照的一致性保护。

若员工选择保留此前配对工具消息支持，必须完整校验当前 Provider 契约，至少函数名、arguments 形状及 JSON、ID/类型、结果归属与序列都合法；不能继续以“对象存在”作为合法性证明。优先采用上面的无工具范围，减少不必要分支。

### 闭合测试必须只有目标故障

当前 T7 的工具反例仍有额外拒绝原因：未配对样本缺 content/完整函数；object 型 tool_calls 样本没有 user，即使错误地忽略其类型，也会被 `saw_user=false` 拦住。对应 [session_turns.rs:1439](../../../../NoManCode/rust-app/tests/session_turns.rs#L1439)、[session_turns.rs:1462](../../../../NoManCode/rust-app/tests/session_turns.rs#L1462)。

从正常完成的前缀各建独立夹具，每次只改变目标字段。覆盖空历史、非法 content、对象型 tool_calls、带 user 与合法 content 的不完整 function、未配对调用；若采用无工具方案，再测结构完整且已配对的工具历史也明确不支持。每个失败均断言准确错误、无 Turn/Task/幂等/事件增加、无活动任务、Provider 调用不增加；保留纯文本连续三轮成功用例。

## 5. B-R5 · P2：仍未成立的验收证据及具体改法

本轮已承认的进展：T8 增加让出调度和 `!is_busy()`；T10 真实 resume 后旧候选冲突有效；T11 用短新消息验证累计上下文超限有效。以下要求延续上一轮，不新加产品范围。

### 5.1 T9：目前没有当前回合的 partial，也没有停止旧执行器

入口：[session_turns.rs:36](../../../../NoManCode/rust-app/tests/session_turns.rs#L36)、[session_turns.rs:932](../../../../NoManCode/rust-app/tests/session_turns.rs#L932)。

mock 对 `stream-partial` 只是等 30 秒后一次性返回字符串；在此之前没有发出部分 SSE。测试只等 running 就 drop Harness，而 Engine 启动的后台任务持有自己的 Arc，drop 外部 Harness 并不会停止它。最后检查整个 legacy Run 中是否有任意 delta，首轮回答足以满足该断言。

负责人在同一夹具中等待新请求真正到达 mock，得到：

```text
run_deltas=1, current_task_deltas=0, old_executor_alive_after_drop=true
```

所以“真实部分流 → 关闭旧运行 → 保留该轮草稿”的报告结论仍不成立。这条反例检验的是测试前置条件，**不据此宣称生产 recover 本身已经丢草稿**。

**具体实现步骤：**

1. 把 mock 响应改成真实分块 SSE，例如 `Body::from_stream`：先发包含唯一文本 `partial-b-r5` 的 delta，再等待 Notify/gate；不要先 sleep 再返回完整 String。
2. 等待目标 Task/Turn 的 delta 已落库，断言事件 task_id 正是新 Task，文本确实包含该标记。不能只等 running 或查整个 Run 的任意事件。
3. TempDir 由测试外层持有。把执行 Engine 放在可独立销毁的运行单元中，例如专属线程上的 Tokio Runtime；收到目标 delta 后，让该运行单元退出并真正 drop Runtime，等待线程结束。后台任务停止后才重开 Store/Engine。单独 drop Arc 或取消业务任务都不能冒充崩溃重启。
4. 重开后 recover：目标任务 interrupted，Task.output 含 `partial-b-r5`，Turn/Task/route/model 不变；恢复和同 key 回放不增加 mock 调用。显式 resume 才再次调用。
5. 已持久 cancelled、只提交 queued 未 launch 是另外两个夹具，保留并分别断言。TempDir 不随旧 Harness 一起释放，不能依赖 Windows 尚有打开句柄导致目录删除失败。

不要求写复杂测试框架。一个外层临时目录、一个可停止 Runtime、一个分块 mock 和几个明确通知足够。使用超时保证失败可诊断，不无限等待。

### 5.2 T3/T5：同时发起请求与内部串行提交是两回事

入口：[session_turns.rs:398](../../../../NoManCode/rust-app/tests/session_turns.rs#L398)、[session_turns.rs:415](../../../../NoManCode/rust-app/tests/session_turns.rs#L415)、[session_turns.rs:611](../../../../NoManCode/rust-app/tests/session_turns.rs#L611)。

当前 Engine 用例先 await 第一次 `send_chat_turn` 返回，再 spawn 第二次，证明的是顺序回放。两个 Store 线程仍没有开始屏障。

报告中的“内存 gate 串行，所以不能验证 Engine 并发”理由不成立：锁本来就应该串行临界区，两个调用方仍可以同时发起请求；两个独立 Engine 还有各自的锁。之前死锁来自单线程 Runtime 上的同步 `recv()`，不是双 spawn 本身。

**具体步骤：**

- 给两个工作任务和协调者一个异步 Barrier，两个任务都就绪后同时释放，分别调用同 key 的命令，再等待两个回执。不要等第一次返回后才创建第二次。
- 同时断言 Turn/Task ID 一致，只有一个 `replayed=false`，新消息在 mock 中只出现一次，数据库与初始事件只新增一组。
- 独立 Engine 用 `Engine::new(Arc::new(Store::open(同一数据库)), ...)`，复用已有设置，不调用会覆盖配置的 fixture 初始化函数。
- 跨 Store 的同 key/不同 key 场景保留，用线程 Barrier 同步入口；SQLite 事务本身应串行，不要求两个写事务同时持锁。

### 5.3 T6：固定反向 ID，不再用顺向的 zzzz 新回合

入口：[session_turns.rs:657](../../../../NoManCode/rust-app/tests/session_turns.rs#L657)。较新 Turn 被改成 `zzzz-later-looking`，它比旧 UUID 大，字典序仍与提交顺序相同。错误的 created_at/id 排序也能通过这部分测试。

把旧 Turn 固定为 `zzzz-old`，新 Turn 固定为 `aaaa-new`，两者 created_at 相同；同步更新临时夹具的关联引用。明确先断言 `old.id > new.id` 且首事件 seq 正向，再断言 latest=new、旧请求 StalePredecessor。保留已有效的旧事件追加反例。

### 5.4 T7/T11：独立夹具，不能用另一个错误遮住目标守卫

| 场景 | 当前问题 | 精确修法 |
| --- | --- | --- |
| workspace 不符，[session_turns.rs:1506](../../../../NoManCode/rust-app/tests/session_turns.rs#L1506) | 使用已经增加第二 Agent 的 `multi`。当前 Engine 路径预检会拒绝，但移除路径保护后仍可能被 Agent 计数挡住，不能独立证明 Store 路径守卫 | 另建一个合法单 Agent 夹具，只改前序 workspace；同时经 Engine 和直接 Store 提交验证拒绝。Store 反例保持候选与错误前序的路径/消息一致，让快照比较也不能抢先成为拒绝原因 |
| route 删除，[session_turns.rs:1256](../../../../NoManCode/rust-app/tests/session_turns.rs#L1256) | 复用刚注入超长历史的原 harness，先命中 InvalidInput，未执行 Engine::key | 使用新的合法 completed 前序和短历史，先确认候选其他条件满足，再只删除 route；不能仅用任意 is_err 冒充目标分支，核对确由路由保护失败，并断言无副作用/调用 |
| resume 运行中追加 | T10 验证完成后的旧候选有效，但运行中场景仍没有真实时序，busy 样本只是 SQL 改状态 | 用可控 mock 让真实 resume 进入 running，再提交原最新 Turn 的追加，得到 SessionBusy 且零新增；释放 mock 后验证正常完成 |

第 1.2 轮报告按实际命中的分支记录，不能只写“所有 T1～T12 已验证”。

## 6. B-C1：Agent 特例修好，Session/Turn 仍靠中文文案分类

保留上轮的 **非独立阻断诊断建议** 等级。位置：[repository.rs:780](../../../../NoManCode/rust-app/src/repository.rs#L780)、[repository.rs:793](../../../../NoManCode/rust-app/src/repository.rs#L793)。

新 `session_lookup`、`turn_lookup` 使用 `error.to_string().contains("不存在")`。底层旧查询对所有读取错误都加“会话不存在/回合不存在”的 context，因此即使行存在、只是 created_at 负数无法解码，仍映射为 NotFound。实际探针：

```text
Agent 负时间 → CorruptState（本轮已修好）
Session 负时间 → NotFound
Turn 负时间 → NotFound
```

本轮报告“NotFound 现在只表示确定不存在”的范围过大。调用者不应解析文案，Repository 内部也不要用该办法区分数据库结果。

**具体改法：** 使用 QueryReturnedNoRows 的类型或 OptionalExtension 确认真正没有行；对行解码故障使用 CorruptState 并保留底层原因链，busy/locked/其他存储故障继续保留实际原因。复用现有 row 解码函数，不为每个实体复制一套稍有差异的 SQL 解码。缺首事件保持 CorruptState。无需改 protocol 或 C 的 HTTP 文件。

可以在第 1.2 轮顺手补齐；若暂缓，报告明确限于哪些实体，不能继续声称完整修好。在新 HTTP 写入口冻结前必须统一其真实错误语义，不把它升格伪称为本轮新的 P1。

## 7. 第 1.2 轮放行标准与提示词

本轮无需推倒已修好的顺序、快照或归属实现。第 1.2 轮重点是：B-R3 拒绝真实畸形前缀；B-R5 的每个测试建立有效前置条件并记录真正命中的分支；C1 如实修正实现或说明。原有 94 项与新增有效测试应全部完成，仍不执行付费 live、不改 C 文件。

```text
你是员工 B，继续 Engine 连续 Chat 回合。目录 ./。

先完整阅读：
./niuma\员工B\审查记录\第一轮\员工B review 1.1.md
./niuma\员工B\审查记录\第一轮\员工B第1.1轮审查附件\README.md
沿用原任务单、项目开发守则与 A 已验收基线。

B-R1、B-R2、B-R4 实现已闭合，保留现有修复。
本轮 CHANGES_REQUIRED 主要针对 B-R3 和 B-R5：
1. 按 review 的无工具范围修好上下文校验，拒绝配对但 function 不完整等畸形历史。
2. mock 真正先送 partial，再停止旧 Runtime/运行单元，重开后核对当前 Task 的草稿。
3. 补真实同时发起的幂等请求、开始屏障、反向 ID、独立 workspace/route 删除夹具、
   真实 resume 运行中的追加；不能用无关错误提前失败来证明目标分支。
4. B-C1 仍是诊断建议，Session/Turn 不要靠“不存在”文案分类；补齐或如实说明边界。

负责人已在 review 给出修改入口、反例、同步步骤和副作用断言。
直接完成实现和测试，不只改报告，不删除或放宽断言，不自行宣布通过。
C 正在返工，不改 server.rs、http_contract.rs、C 文档，不接新 Turn HTTP 写接口；
不改 schema/protocol、不全仓自动格式化、不提交他人的改动。

报告写到：
./niuma\员工B\提交报告\第一轮\员工B（Engine连续Chat回合 第1.2轮报告）.md

保留历史报告，逐项沿用 B-R1～B-R5 编号。写明代码、有效夹具、实际错误、持久化/调用断言、
命令与退出码、宿主路径、未完成项；不能把“计划覆盖”写成“已验证”。
```
