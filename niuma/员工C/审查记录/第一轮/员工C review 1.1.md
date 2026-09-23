> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 C 第 1.1 轮复审：HTTP 与 SSE 传输契约

日期：2026-09-23  
审查人：项目负责人  
结论：**CHANGES_REQUIRED，仅剩 C-R5 的 H7 测试同步问题需要收尾。C-R1～C-R4 已闭合。**

依据：[C 第 1.1 轮报告](<../../提交报告/第一轮/员工C（HTTP与SSE传输契约 第1.1轮报告）.md>)、[上一轮审查](<员工C review.md>)、[任务单](../../任务/第一轮/员工C提示词.md)、[开发守则](../../../../项目开发守则.md)。  
附件：[复验源码、日志和使用说明](员工C第1.1轮审查附件/README.md)。

## 1. 本轮结论

传输实现的四类已知问题都已修复。当前正式门禁为 **94 项通过、1 项付费测试忽略**，fmt、Clippy、WASM 检查均通过；上轮 7 个审查探针也全部通过。没有发现需要再次修改 server 的阻断项。

尚不能结束本轮验收的原因很小且明确：H7 新增的真实运行夹具使用 `notify_waiters()` 发一次握手通知，接收方却可能稍后才开始等待。合法的调度顺序会让通知丢失，导致测试在连接 SSE 之前超时。隔离反例已复现；仅将该通知改为 `notify_one()` 的对照完整通过。

这是**测试稳定性问题，不是生产 SSE 断线取消任务的问题**，也不否定原版门禁这次全部通过。下一轮聚焦此项，保留已闭合的生产实现与已有断言。

## 2. C-R1～C-R5 的状态

| 编号 | 复核结果 | 状态 |
| --- | --- | --- |
| C-R1：编码游标 | [query_cursor](../../../../NoManCode/rust-app/src/server.rs#L739) 先严格解码名称，再识别和计数 after；query/header 都先验证，然后按优先级选择。原编码重复与编码非法值 HTTP 反例均返回 400，两种 SSE 共用解析器 | 闭合 |
| C-R2：读取错误分类 | [read_error](../../../../NoManCode/rust-app/src/server.rs#L215) 对纯读取中的普通一致性错误兜底安全 500；确认缺行仍 404，冲突仍 409。settings 读取与随后缺设置/找不到路由的业务 400 已分开；Run 身份损坏反例通过 | 闭合 |
| C-R3：路径错误 | 全部 12 个含路径参数的既有 handler 已接收 `Result<Path<String>, PathRejection>`，经 object_id 转统一 JSON。正式测试覆盖 Session、Turn、Run 和两种 SSE 的非法 UTF-8 路径；原反例返回 JSON 400 | 闭合 |
| C-R4：SSE 编帧 panic | [sse_frame](../../../../NoManCode/rust-app/src/server.rs#L838) 在 builder 前校验 kind；读取或构帧失败发一次 error 后结束，游标不越过失败事件。坏 kind 与坏 JSON 两个同流反例均通过 | 闭合 |
| C-R5：有效测试证据 | H7 已真实运行任务；H8 已同名 A1/B1/A2 交错；H9 已使用真正未来游标；H10 已在同一 Response 收到正常帧后故障注入。H7 新握手仍有丢通知竞态 | 仅 H7 同步待修 |

health 的负 user_version 夹具已纳入正式测试，真实 HTTP 返回 500/internal/retryable=false，且没有 ok/schema_version 成功字段。H10 已撤回上轮不成立的证据描述，并验证 error 的 after、无 id、EOF、记录数和 Provider 调用数。

## 3. C-R5 · P2：H7 单次握手可能丢失

### 触发条件与实际结果

相关位置是 [发送通知](../../../../NoManCode/rust-app/tests/http_contract.rs#L1009) 和 [创建等待](../../../../NoManCode/rust-app/tests/http_contract.rs#L1078)。目前顺序允许：

```text
测试发起 POST /api/runs 并等待响应/JSON
→ Provider 进入，calls += 1，started.notify_waiters()
→ Provider 等待 release，任务仍在执行
→ 测试这时才创建 entered.notified()
→ 没有下一次 started 通知，3 秒后超时
```

`notify_waiters()` 不为稍后创建的等待保存通知。把 timeout 增大、增加 sleep 或多重试几次都不能解决这个顺序下的丢失。

负责人在隔离副本复制完整 H7，只在收到 POST 的 Run 后、创建 entered 等待前，加一个有 3 秒上限的 `yield_now()` 循环，直到 `calls == 1`。原测试采用单线程 Tokio runtime，Provider 从增加 calls 到发送通知之间没有 await；因此这固定了现有异步执行本就允许的“先通知、后等待”顺序，并未改变生产代码或伪造任务状态。

结果：

| 探针 | 实际结果 |
| --- | --- |
| `review_h7_delayed_waiter_original`：保留 notify_waiters | 在 entered 等待处得到 `Elapsed(())`，失败 |
| `review_h7_delayed_waiter_notify_one_control`：仅换成 notify_one | 完整通过，包括断开仍 busy、释放后 completed、调用一次、重连 seq 增大 |

日志：[review-h7-schedule.log](员工C第1.1轮审查附件/review-h7-schedule.log)。两个探针合计 1 通过、1 失败；它们是额外隔离检查，不能写成“正式 11 项 HTTP 测试中有一项失败”。

### 具体修改思路

这沿用 C-R5，是补齐夹具时引入的同步问题；上轮的“任务已经结束才断开”前置条件已经修正，不应重新判回原问题。

本轮是一对一、一次性的进入握手，可以采用已在对照中验证的最小修法：

```rust
counted.fetch_add(1, Ordering::SeqCst);
started.notify_one(); // 接收方稍后等待时仍可消费保存的 permit
gate.notified().await;
```

保留 `entered.notified()` 的有界等待。也可改用 oneshot channel，但不需要为本问题引入依赖或修改生产逻辑。释放端当前路径已在 Provider 等待后才触发；若同时改为一对一通知，说明理由并保留全部完成断言。

正式回归要固定一次“Provider 先到、测试后等”的交错。可按附件在 entered 等待前有界 yield 到 calls 为 1，再等待 entered，随后执行完整 H7。仅多跑原测试无法证明竞态消失。必要时将 H7 抽成小辅助函数覆盖两种先后顺序，避免复制整份审查附件进入正式测试。

闭合条件：强制晚等待不超时；消费事件后断开时任务仍 busy；释放 mock 后任务完成；Provider 总调用仍为 1；重连收到更大 seq。不能删除 entered 等待、忙状态或完成断言来制造通过。

## 4. 实际验证与基线

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`，连同 A/B/C 未提交的有效工作区修改一起复制审查。

固定源码目录：`./.local/temp/fufu-c11-review-e80a349693`。复用负责人已有且当时空闲的隔离 target 缓存，没有使用或清理员工正在使用的 target。审查结束复核时，server、http_contract 和记录的 5 个 B 文件 SHA256 均与快照一致，清单在附件。

| 检查 | 结果 | 证据 |
| --- | --- | --- |
| `build.ps1 -Action test` | 94 通过、0 失败、1 付费 live 忽略，退出 0；其中 C 的 HTTP 为 11 通过 | review-test.log |
| `build.ps1 -Action fmt` | 退出 0，无输出 | review-gates.json |
| `build.ps1 -Action clippy` | 退出 0 | review-clippy.log |
| `build.ps1 -Action wasm-check` | 退出 0 | review-wasm-check.log |
| 上轮 7 个 HTTP 审查探针 | 7 通过、0 失败，退出 0 | review-probes.log |
| H7 强制交错与单点修改对照 | 原版通知失败；notify_one 对照通过，合并退出 101 | review-h7-schedule.log |

正式四项门禁在添加隔离探针前执行，不能把探针对照的改动算作交付代码。没有执行付费 Provider 调用。上述脚本使用真实 PowerShell 7.6.5；仅出现原有 proc-macro-error2 未来兼容提示。

C 报告没有冒称全量通过，但引用的 B 中间态阻塞已过时：B 第 1.1 轮和本次 C 固定快照均已走完全量。后续报告应按当时快照实跑；若又受并行修改影响，记录具体新证据，不能继续沿用旧阻塞作为未执行理由。B 的功能验收仍按 B review 1.1，测试全绿不改变其未闭合项。

## 5. 非阻断说明与范围

- H9 在阈值处通过 Store 断言无结果，随后同一 HTTP Response 的首事件必须大于 future，已足以闭合本轮反例。两次追加间没有 await，尚未单独观察 HTTP 在阈值处的等待窗口；可后续补强，不要求本轮扩展。
- `LiveSse` 的 pop_data_frame 跳过注释后会返回 None，外层直接继续读网络；若同一 chunk 已有注释和数据，可能漏读剩余缓冲直到下次网络输入。当前 H7/H10 在 keep-alive 前完成，本次不以此阻断。以后扩展 keep-alive 测试时应循环消耗缓冲中的完整注释帧，再判断是否需要读网络。
- 解码后参数名含字面 `%` 也被拒绝，如 `other%25=1`；它比标准单次解码后的未知参数忽略更严格，契约已记录。没有影响当前定义的 after 客户端，不扩展为本轮生产返工；新报告应明确这个例外，避免笼统说所有未知参数都不拒绝。

本次只复审、隔离复现和写文档；没有修改共享 Rust 实现、正式测试或 B/C 报告，也没有提交或发布。C 下一步可只改自己的 HTTP 测试和报告。B 仍按其第 1.2 轮任务返工，新增 Turn HTTP 写入口继续等待联合任务，不因 C 生产修复闭合自动开放。

## 6. 可直接分发给员工 C 的第 1.2 轮提示词

```text
你是员工 C，工作目录 ./。

先读取《项目开发守则.md》，然后完整读取：
./niuma\员工C\审查记录\第一轮\员工C review 1.1.md
./niuma\员工C\审查记录\第一轮\员工C第1.1轮审查附件\README.md

本轮只收尾 C-R5 的 H7 同步问题。C-R1～C-R4 已闭合，H8/H9/H10 和 health 场景已认可，不重写 server，不扩大需求。

H7 的 started.notify_waiters() 会丢失先通知后等待的单次握手。负责人已用强制 Provider 先进入的反例复现，并实跑仅替换 notify_one() 的对照通过。按 review 第 3 节采用最小修法，或等价的一次性 channel。

在正式回归中固定“Provider 已通知、测试随后等待”的交错，不仅依赖多跑。保留原 H7 的真实运行、断线仍 busy、释放后完成、调用总数1、重连游标增大等全部断言；不要增加长 sleep、删除等待或降低断言。

仅修改 NoManCode/rust-app/tests/http_contract.rs 和自己的报告；如需修正文档说明可改自己的 HTTP-SSE 契约。不要动 B 的 Engine、Store、Repository、domain、session_turns；不改 schema/protocol，不加 Turn 写路由，不全仓自动格式化，不提交或发布。

先检查并保留已有工作区修改。运行定向 H7 和 HTTP 测试，再按项目脚本执行 test/fmt/clippy/wasm-check，记录命令、退出码和实际数量。真实 PowerShell 7.6.5 路径在 review 附件。不要启用付费 live。

当前负责人快照已全量94通过、1忽略，旧 B 中间态阻塞不再适用；若最新快照出现新问题，按实际证据报告并保护对方文件。不要将门禁通过称为整体验收通过。

新报告写到：
./niuma\员工C\提交报告\第一轮\员工C（HTTP与SSE传输契约 第1.2轮报告）.md

保留历史报告；说明本次只处理测试握手及强制调度回归，列清修改文件、验证结果和剩余边界。完成后返回报告路径，不自行宣布审查通过。
```
