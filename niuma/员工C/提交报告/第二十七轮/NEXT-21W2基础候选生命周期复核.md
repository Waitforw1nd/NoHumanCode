# NEXT-21W2 基础候选生命周期独立复核

2026-11-01 · 员工 C / 6.1 Terra。审查固定 `c58ac3909699db8cce62f1cf3b44b0644be2af5a`，产品树 `80a962312bea1bedfa637dbe80a5ae156fa4cc34`、Rust 树 `c0a42cf7d47746e3641bf6c3dad70f98d9d7954f`；以下行号均指该提交的 `NoManCode/rust-app/src/wasm_worker_process.rs`，不套用 B 后续修改的行号。仅静态审查，未运行进程或测试。

## 具体发现

1. **P1：取消 token 不能独立关后台准入，已取消仍可能启动。** `call:85-125` 在启动监督线程前没有检查外部 `CancellationToken`；`Gate::stopped/authorize:193-211` 和 `Stopper:694-719` 只读 gate/deadline，不读 token。传入已取消 token 时，监督线程可能先于 `select!:128-142` 的取消分支运行，通过 `ensure_open`/`authorize` 到 `CreateProcessW` 或 `ResumeThread`；同理 spawn 后若 future 未再获得调度，token 已取消但 gate 暂不关闭。响应仍受接受门阻止且最后 deadline 会停，但这不能保证取消后不发新创建/执行票。建议 spawn 前拒绝已取消，并让 token 的取消观察能独立关闭同一 gate、与既有已授权调用及资源存续配合；用可控停点覆盖预取消、future 暂停后取消、已授权 create/resume 迟到，而非声称已授权 resume 后绝无指令。
2. **P1：检测到真实不属于 Job 后只终止 Job，目标进程可能无法收回。** `IsProcessInJob` 返回成功且 `inside==0` 时 `:1155-1157` 返回错误，随之 `ProcessGuard::drop:898-903` 调用 `wait_reaped:890-895`；它只对 Job 调 `TerminateJobObject`，而 Microsoft [TerminateJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-terminatejobobject) 作用于 Job 关联进程。如果此分支真实出现，刚创建的目标仍挂起但不属于 Job，Job 终止触不到它；process signal 不到，循环持续、slot 保留 unknown。建议同时对可信 process handle 请求终止，并分别真实等待 process signal 与查询 Job 活跃数为 0；任何失败继续占槽。此为异常防护分支，不证明正常 W01 的原子归属必然失败。
3. **P1：I/O 观察失败被统一当终态，可能过早释放内核仍使用的存储。** `Operation::poll:827-842` 对 `GetOverlappedResult` 除 `ERROR_IO_INCOMPLETE` 外所有失败都置 `pending=false`；之后 `Drop:845-853` 不再等待，释放 OVERLAPPED、buffer、event 和 pipe。`Pipe::create:643-663` 对 pending connect 的观察结果也在非 incomplete 错误时直接返回并丢弃 OVERLAPPED。正常的已完成错误可释放；若是观察 API 自身失败、无法证明对应 I/O 已完成，不能仅由“不等于 IO_INCOMPLETE”推断内核已不再访问内存。Microsoft [CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex) 明确取消请求之后直到操作完成前不得释放或复用 OVERLAPPED，[GetOverlappedResult](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult) 的 FALSE 返回需解释错误。应区分已证明的完成终态与无法观察的失败；后一种保留缓冲/句柄和槽并标 unknown，补可控观察失败/取消竞态注入。这里指出缺失的证明及其潜在内存安全影响，未断言有效句柄的正常 Windows I/O 必然产生观察错误。

经理已收到三项并交 B 按其原写入范围处理。B 另行补充 Job breakaway 标志检查，不是本报告新增结论。

## 其他核查与验证范围

- oneshot 已由接收分支消费时，`received_completion` 令 `finish_cleanup_wait:155-165` 直接返回，避免对已完成 receiver 再 poll；未消费时有 2 秒等待，超时返回 `CleanupUnknown`。future Drop 的 `CancelOnDrop:214-220` 会关闭 gate。`call:116-123,168-172` 在线程正常返回后释放进程全局 slot；超时但监督线程仍在等待资源则继续持槽。线程 panic 时 receiver 断开返回 `CleanupUnknown`，但 `SlotLease` 无条件 Drop；安全性依赖展开时所有操作/进程守卫确实取得完成证据。第三项观察失败路径正是需要补强的相关证明缺口，不能笼统声称 panic/错误均已证安全。
- 接受与 `stop()` 通过 `admission` mutex 和 gate CAS 协调；外部 token 检查与 CAS 可能重叠。若规范将持锁且后续 CAS 成功的 token/time 检查时刻定义为接受线性化点，可把该重叠排序为先接受；仅凭重叠本身不能断言已证错误。建议明确 deadline/取消的线性化时刻并以确定性停点测试，不与第一项已取消 token 尚未关门的问题混同。
- Operation 在正常 pending 路径持有 buffer、event、OVERLAPPED 和 pipe，Drop 循环请求取消并收割；StopOnFailure 先关 gate，ProcessGuard 等待 process signal 和 Job active=0。已授权 create、I/O submit、resume 可能在取消后迟到，因此不能以 CancelIoEx 返回、超时或 token 状态直接释放；需要实际 fault injection 和回收证据。`CreateProcessW` 成功但 process handle 无法取得时的 park 循环保持占槽，是保守阻塞，不是已回收。
- 固定候选 W01 测试本身尚未执行；W02–W06 协议故障、堵塞管道、资源/取消/unknown、真实 Host 强杀及工件/继承反例尚待实现或动态证明。以上真实缺口须修正，但不单凭后续矩阵未完成而否定经理安排的**有限 W01** 基础可行性窗口；W01 成功也不等于生命周期矩阵、W2 生产接线或 W3 恢复通过。

本轮只读源码与 Microsoft 原始文档，未修改产品、暂存、提交、构建、测试或启动进程；已核候选三 SHA。下一步等待经理和 B 完成增量修正与范围内复核，本人释放。
