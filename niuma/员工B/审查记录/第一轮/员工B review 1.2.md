> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 第 1.2 轮复审：通过

日期：2026-09-23

审查人：项目负责人

结论：**APPROVED（已有单 Agent、无工具 Chat Session 的连续 Turn 应用服务）。B-R1～B-R5 全部闭合，B-C1 原诊断反例也已闭合。**

依据：[B 第 1.2 轮报告](<../../提交报告/第一轮/员工B（Engine连续Chat回合 第1.2轮报告）.md>)、[第 1.1 轮复审](<员工B review 1.1.md>)、[原任务单](../../任务/第一轮/员工B提示词.md)、[开发守则](../../../../项目开发守则.md)。

最终交接：[员工 B 最终审查](<员工B 最终审查.md>)。证据：[本轮审查附件](员工B第1.2轮审查附件/README.md)。

## 1. 放行依据

本轮完成了前次批准的无工具历史收窄，并补齐真正的部分流重启、同时发起请求、逆序 ID 和独立故障夹具。原 B-R1/B-R2/B-R4 的事务与归属实现保持，没有重写已闭合部分。

负责人隔离复验：**99 项正式测试通过、0 失败、1 项付费 live 忽略**，fmt、Clippy、WASM 检查全通过；8 项仍适用的历史探针和 2 项新增边界探针全部通过。未发现需要继续阻止本轮交付的问题，不要求第 1.3 轮返工。

这个结论允许下一阶段复用 `Engine::send_chat_turn`，不代表新 HTTP 写接口、工具历史、持久调度或整个产品已经完成。

## 2. 问题逐项关闭

| 编号 | 本轮证据 | 结论 |
| --- | --- | --- |
| B-R1：提交顺序与历史保护 | 每轮 MIN(seq) 算法未变；正式 T6 固定 zzzz-old/aaaa-new 的反向 ID 与同秒时间，验证首事件顺序、陈旧前序拒绝及旧事件不改变 latest；历史审查探针也通过 | 闭合保持 |
| B-R2：前序快照竞争 | 立即事务内比较真实前序与候选的逻辑未变；合法 resume 完成后旧候选仍返回 PredecessorChanged；新增真实运行中 resume 场景返回 SessionBusy | 闭合保持 |
| B-R3：畸形/工具上下文 | Engine 预检和 Store 事务内共用 tool_free_chat_prefix；不完整或结构完整的配对工具历史都在新增与调用前拒绝；正常纯文本和显式空工具数组通过 | 闭合 |
| B-R4：Agent/workspace 归属 | 原保护保持，独立单 Agent 错误 workspace 夹具同时验证 Engine 和 Store，候选跟随错误前序，避免另一个守卫提前遮住目标分支 | 闭合保持 |
| B-R5：有效验收证据 | 分块 partial、旧 Runtime 停止、并发屏障、反向 ID、独立 route/workspace、真实 resume running 均实际成立并通过 | 闭合 |
| B-C1：损坏误报不存在 | Session/Turn 不再解析中文文案；仅 QueryReturnedNoRows 为 NotFound；负时间探针均为 CorruptState，保留底层原因链 | 原反例闭合 |

## 3. B-R3 与 B-C1 的具体核对

[domain::tool_free_chat_prefix](../../../../NoManCode/rust-app/src/domain.rs#L569) 要求非空文本历史、至少一条 user，角色限 system/user/assistant。`tool_calls` 缺失或显式空数组允许；非空数组、非数组及 role=tool 拒绝。不裁剪字段、不补伪结果，也不引入完整工具消息协议。

[Engine 预检](../../../../NoManCode/rust-app/src/engine.rs#L395) 在构造新回合、提交和 launch 前执行；[Store 校验](../../../../NoManCode/rust-app/src/store.rs#L658) 在立即事务内检查真实前序，随后仍做快照比较。原 `function:{}` 加配对结果反例本轮结果为：

```text
incomplete_function_accepted=false
provider_call_delta=0
turn_delta=0
```

正式新 T7 每种故障独立构建正常会话，核对 UnsupportedSession、六类实体计数、Provider 调用和非 busy。六类实体计数并不包含 events、turn_tasks 和幂等表，不能把它写成“全部表已断言”。负责人另补两个隔离探针：

- 不完整及完整的配对工具历史分别经过 Engine、直接 Store 追加，均准确拒绝；projects/sessions/agents/runs/turns/tasks/turn_tasks/turn_task_dependencies/idempotency_records/idempotency/events 共 **11 张表计数保持**，旧前缀保持、Provider 不增、无活动任务。
- `tool_calls: []` 的正常文本前缀可完成新回合，原前缀保持，Provider 只新增一次调用。

[session_lookup / turn_lookup](../../../../NoManCode/rust-app/src/repository.rs#L780) 复用既有行解码函数；[classify_row](../../../../NoManCode/rust-app/src/repository.rs#L1450) 只把确认缺行分为 NotFound，将 FromSqlConversionFailure / IntegralValueOutOfRange 以 CorruptState context 包装，保留 rusqlite 原因链。Agent 的原负时间探针也继续通过。

没有声称所有存储错误都已变成 ChatTurnError。例如 InvalidColumnType 等仍可能作为原始存储错误传播；后续 HTTP 接入必须保留安全 500，不能因为不是已知枚举就映射成 404 或统一业务 400。

## 4. B-R5：已成立的测试前置条件

| 场景 | 实际执行路径与关键断言 |
| --- | --- |
| 真实部分输出后重启 | mock 用 Body::from_stream 先发 partial-b-r5，再等通知。T9 等待当前 task_id 的唯一 delta 已落库；独立线程内 Runtime 被 drop，join 完成后才重开 Store。该任务 interrupted、草稿保留、模型保持，同 key 回放不 busy、不多调用 |
| 独立 Engine 同 key 并发 | 两个 Engine、同一数据库、各自内存 gate；异步 Barrier 让两个调用方同时就绪。只有一个 replayed=false，Turn 一致，目标消息只调用 Provider 一次 |
| 独立 Store 同 key/不同 key | 两个连接通过线程 Barrier 同时进入；同 key 一个创建一个回放且 Turn/Task 相同，不同 key 只有一个成功、另一个为 StalePredecessor |
| 同秒、反向 ID | 旧 zzzz-old 大于新 aaaa-new，但旧首事件 seq 更小；latest 取新回合，追加旧前序失败，补旧事件与旧 resume 不能重开历史 |
| route 删除 | 新的合法短前缀，只删配置 route；失败明确来自 key 的路由保护，不再被累计上下文超限提前遮住 |
| workspace 不符 | 另建单 Agent 会话，只改前序路径；Engine 与直接 Store 均拒绝，Store 候选与错误前序保持一致 |
| 真实 resume 运行中 | 真正 resume 并等到当前 Task running 且 resume-ok 已落库，再追加得到 SessionBusy；释放 mock 后该 resume 完成 |

主要入口：[分块 mock](../../../../NoManCode/rust-app/tests/session_turns.rs#L35)、[Engine 并发](../../../../NoManCode/rust-app/tests/session_turns.rs#L440)、[反向 ID](../../../../NoManCode/rust-app/tests/session_turns.rs#L698)、[partial Runtime](../../../../NoManCode/rust-app/tests/session_turns.rs#L994)、[单故障 T7](../../../../NoManCode/rust-app/tests/session_turns.rs#L1708)、[route/workspace](../../../../NoManCode/rust-app/tests/session_turns.rs#L1772)、[运行中 resume](../../../../NoManCode/rust-app/tests/session_turns.rs#L1876)。

上轮 `review_b11_partial_fixture_must_have_its_own_delta` 是专门验证旧“drop Harness 就算重启”的过程。该过程已被新的正式 T9 替换。本轮明确排除这个旧过程探针，改查并运行当前的 Runtime 停止链路；不是删除正式断言以掩盖失败，也不能把旧探针本身仍 drop Harness 的写法用来判定新 T9 未修。

## 5. 门禁与基线

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`，连同有效未提交 A/B/C 修改审查，不能仅检出该提交复验。

固定源码副本：`./.local/temp/fufu-b12-review-5494090598`。真实 PowerShell 7.6.5，pwsh 解析为既有 runtime 程序，负责人没有向 PATH 添加 Windows PowerShell shim。使用本机 mock 和临时数据库，未执行付费 live。

| 检查 | 结果 | 附件 |
| --- | --- | --- |
| build.ps1 -Action test | 99 通过、0 失败、1 忽略，退出 0 | review-test.log |
| build.ps1 -Action fmt | 退出 0，无输出 | review-gates.json |
| build.ps1 -Action clippy | 退出 0 | review-clippy.log |
| build.ps1 -Action wasm-check | 退出 0 | review-wasm-check.log |
| 仍适用的历史探针 | 8 通过，退出 0；17 个正式测试及 1 个旧过程探针被过滤 | review-probes.log |
| 追加边界探针 | 2 通过，退出 0 | review-boundary-probes.log |

99 项构成为：43 lib、4 adversarial、9 final_acceptance、11 http_contract、10 runtime、17 session_turns、5 protocol。额外 8+2 探针不重复计入 99；正式四项门禁在加入探针前完成。既有 proc-macro-error2 未来兼容提示不是本次门禁失败。

结束复核时，8 个记录的关键源码文件与副本哈希一致；与 C 第 1.2 轮验收记录重叠的 7 个文件也全一致。C 已独立通过，B 本轮未改其 server/HTTP 测试。B 报告“这次通过不代表 C 已验收”是当时的范围说明，现状以 C 最终审查为准。

## 6. 非阻断改进与能力边界

以下不要求重开本轮返工，后续触及相关测试时收尾：

1. 正式 T7 可吸收上述 11 表无新增断言与空数组正例；Engine 并发用例可补显式 Task ID 和新增事件组数量对比。目前 Store 并发已比较 Task ID，负责人边界探针已补足拒绝副作用的实际证据。
2. T9 的 partial 子夹具已正确外置 TempDir。旧 cancelled 与 committed-not-launched 子夹具仍 drop 包含 TempDir 的 Harness 后重开库；在 Windows 当前文件顺序下可能依赖打开句柄使目录清理失败。后续也应将这两处 TempDir 提到外层，先释放执行器/Store，再重开库。它是测试可移植性和清理问题，本轮并未出现恢复数据丢失。
3. domain 中原 send_chat_turn_hash 的注释留在新 text_content_ok 上方，可顺手移回正确函数；不影响运行行为。

MIN(seq) 无法证明“只删创建事件、保留后续事件”的损坏，属于前轮已确认的限制，不追加 schema 要求。工具历史仍不支持；分布式恰好一次执行、持久 Scheduler 仍未实现。已提交未 launch 的任务重启转 interrupted，回放不自动重新派发。

## 7. 交接决定

B 第一轮任务结束，无需第 1.3 轮返工。`SendChatTurn`、`ChatTurnReceipt`、`ChatTurnError` 的本轮应用契约可以供后续客户端接入，具体字段与边界见最终审查。

B 与 C 的基础范围现在均已通过，下一阶段是新增连续 Turn 的 HTTP 接入及跨链路验收。新路由尚未实现；HTTP DTO/错误映射需按新任务明确，不能把应用错误按文案判断，也不能借接口接入扩大到工具、Team 或持久调度。

本轮负责人只审查、隔离复验和写文档，未修改共享 Rust 实现/正式测试、员工报告，没有提交、发布或替换运行实例。


勘误补充（2026-09-23，项目经理）：路径整理误改的表名已依据同轮探针恢复为 idempotency_records，仅修复一处表清单文本，验收结论、测试记录与旧哈希均未改。见 [勘误与接手复核](../../../项目经理/审查记录/2026-09-23接手演练与协作收尾.md)。
