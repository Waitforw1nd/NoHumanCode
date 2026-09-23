# 员工 C review 2.0：新增 Turn HTTP

日期：2026-09-23。审查人：项目经理。任务：C-R2-01 / NEXT-01 修订1。关联报告：[员工 C 第2.0轮报告](<../../提交报告/第二轮/员工C（新增Turn-HTTP接入 第2.0轮报告）.md>)；任务契约：[新增 Turn HTTP 契约](../../任务/第二轮/新增Turn-HTTP契约.md)；证据目录：[NEXT-01证据](../../提交报告/第二轮/NEXT-01证据/)。

## 结论

**CHANGES_REQUIRED（证据补强后再验收）**。

经理只读核对确认：当前 `server.rs` 与 `tests/turn_http.rs` 的 SHA-256 分别为 `d89f035c0fdb8ca8b3e316e73e2d8f80a3e134d535e6a66e743d20f1f9959667` 与 `430a6f9e8792d95f0e9d95b02736d4a71a7c6242a543024167f4737638f629c8`，与 C 报告一致；路由、严格 DTO、唯一 Idempotency-Key、`turn_command_error` 的 typed downcast 和 Engine 单次调用顺序符合契约。经理没有重新运行产品测试，因此报告中的全量测试结果仍是员工证据，不是本次经理复跑结果。

当前不能放行的原因是验收测试的前置快照和故障隔离不足，不能证明若干拒绝请求没有副作用，也不能证明 N07 的累计消息大小真正跨过 Engine 的 1,500,000 字节阈值。问题集中在测试证据，不要求扩大 C 的源码写入范围；C 仍只可改 `NoManCode/rust-app/src/server.rs`、`NoManCode/rust-app/tests/turn_http.rs` 及自己的报告/身份。

## 必须修订的条目

### P1：N09 的拒绝前快照时序和独立故障不成立

入口：[turn_http.rs:1583](../../../../NoManCode/rust-app/tests/turn_http.rs#L1583) 至 [turn_http.rs:1656](../../../../NoManCode/rust-app/tests/turn_http.rs#L1656)。`foreign_agent`、`foreign_turn`、`team` 和双 Agent 请求都在请求返回后才调用 `snapshot`，所以即使请求已写入，随后快照也会把该写入当作基线，`assert_untouched` 无法发现它。每个请求必须先保存完整 11 表快照和 Provider 调用计数，发送请求，先断言状态/结构化错误，再与请求前快照和调用计数比较。

同一测试 [turn_http.rs:1662](../../../../NoManCode/rust-app/tests/turn_http.rs#L1662) 起连续把 `tools=true`、`role=tool`、非空/错误类型 `tool_calls` 写进同一 predecessor；后两个例子运行时仍携带前一个故障，不能证明各自的分类入口。每个形状故障必须在独立 fresh fixture，或请求后恢复到同一合法基线并用恢复后的前快照重新注入；保留空数组正例，但证明它是在 `tools=false`、首条 user、其它 tool_calls 清洁的前置下执行。

验收断言：每个 N09 负例均满足 `status=400/request_failed/retryable=false`、请求前后 11 表逐单元格相等、Provider 调用差值为 0；每个 tool 形状反例只由本次注入触发，不能依赖残留字段。

### P1：N10 与 N14 缺少请求前副作用证据

N10 的 stale、running、behind 分支位于 [turn_http.rs:1727](../../../../NoManCode/rust-app/tests/turn_http.rs#L1727) 至 [turn_http.rs:1750](../../../../NoManCode/rust-app/tests/turn_http.rs#L1750)，请求前没有保存快照/调用计数；只有 cancelled/interrupted 分支有前置快照。为每个拒绝请求补齐快照和 `calls_before`，并断言事务没有新增 Turn/Task/idempotency/event 或改写旧行，Provider 调用仍为零。忙状态必须保持真实 `hold:` + 已观察 partial delta 的前置，释放动作放在全部断言之后。

N14 的 missing route、missing secret、不可达 base URL 分支位于 [turn_http.rs:2150](../../../../NoManCode/rust-app/tests/turn_http.rs#L2150) 至 [turn_http.rs:2178](../../../../NoManCode/rust-app/tests/turn_http.rs#L2178)，同样缺请求前的完整快照和调用计数。每一分支都要先记录快照，再发新 key 命令，断言 `500/internal/false`、没有 Provider 调用、数据库不变；随后单独验证同 key 回放在配置删除后仍为 `200/replayed=true`，并把恢复配置后的成功命令放在新的前置快照之后。

验收断言：N10/N14 的每个拒绝命令均为事务外无可见写入；回放断言必须使用已存在的同 key 记录，不能把配置失败的新 key 当作回放。

### P1：N12 竞争只断言响应，没有完整表快照

N12 的异 key race 在 [turn_http.rs:1930](../../../../NoManCode/rust-app/tests/turn_http.rs#L1930) 起主要断言一个 201、一个 409 以及失败 key 后续成功，但没有对竞争前后 11 张表做完整逐行比较，也没有把幂等记录、事件和 Provider 调用差值绑定到胜者。补充 barrier 前快照、两响应完成后的快照和调用计数，断言恰一组 Turn/TurnTask/Task/idempotency/idempotency_record/event 由胜者产生，败者请求不写入、不调用 Provider；随后失败 key 的重试作为独立新命令只再增加一组。

验收断言：竞争批次 `after` 相对 `before` 只有胜者的一组新增，两个 key 的身份和请求摘要保持契约定义；失败 key 的 409 不能被后续重试的新增掩盖。

### P1：N07 的累计大小反例没有越过真实 Engine 阈值

N07 在 [turn_http.rs:1365](../../../../NoManCode/rust-app/tests/turn_http.rs#L1365) 注入 `1_460_000` 个 ASCII 字节，然后追加一条消息。Engine 的实际边界是 [engine.rs:400](../../../../NoManCode/rust-app/src/engine.rs#L400) 对 `serde_json::to_vec(messages).len() > 1_500_000` 的判断；现有 fixture 的系统消息、历史消息和 JSON 字段开销不足以把约 1.461 MB 推过 1,500,000，反例与实现逻辑不一致，不能以当前 `400` 报告为有效证据。请改为在请求前直接计算/断言待提交 `messages` 序列化长度确实 `> 1_500_000`，同时保留一个 `<= 1_500_000` 的成功边界；不要用仅依赖字符数的猜测。

验收断言：超限前置应有明确 `serialized_len > 1_500_000` 证据，响应为 `400/request_failed/false`，请求前后 11 表与 Provider 调用不变；边界正例 `serialized_len <= 1_500_000` 能成功并可回放。

### P2：继承/客户端构造证据需准确标注

N16 的 interrupted 分支目前主要断言回放身份，未像 N13 那样在请求前后保存完整副作用快照，可在本轮补证时一并增加。N11 的两段并发在响应后立即读取 `calls_for`，没有等待后台 launch/provider 的可观察信号，存在调度竞态；应改用 `wait_delta` 或有界轮询后再断言调用次数。N05/N06 中 `%FF` 路径和非 ASCII header 有客户端构造即失败的情形；若不增加真实 loopback 原始请求，就在报告中明确这是客户端边界证据，不能写成服务端已返回 400。N03 的“等价 Unicode 转义”说明也要与测试实际发送的 raw body 一致，避免测试名与请求内容错位。

## 文档与环境记录

C 报告第 74 行把 Git 的 `pwsh.exe` 写成机器绝对路径。该路径可作为当时环境事实保留在原始日志，但正文和新证据副本应按仓库路径规范改为相对描述（例如“Git 内嵌 pwsh shim”），不能作为可移植入口。付费 live 仍保持未执行；C 报告的 143 通过、1 ignored 只在补证后作为员工快照引用。

## 复核入口与交付

C 返工只补上述测试前置、故障隔离、边界断言及对应报告/身份记录，不改 Engine/Store/schema/Cargo/UI，不提交 Git。回交时保留本次失败事实，新增报告或修订报告必须列出每个补证命令、退出码、快照哈希和未执行项。经理收到修订报告后再做一次只读审查，并与 D 的冻结候选串行联合门禁；在此之前 NEXT-01 仍是待验收。
