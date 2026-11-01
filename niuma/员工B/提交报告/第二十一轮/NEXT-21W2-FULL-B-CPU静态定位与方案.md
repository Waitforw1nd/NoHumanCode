# FULL-B case04 CPU 静态定位与待裁决方案

2026-11-02，员工 B。固定受测 `82a0016ef1d582abc963270b48149e10fca1220f`、产品 `99ed0add4a89fbee1473a3c511a9972a54148a65`、Rust `8fccff04244c3a3b04e120f7bbb2b6e3d5ecdea4`；本轮只读源码和现场，未构建、测试、重新启动或改代码，原 terminal_unknown 闩锁保留。原始现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-b-82a0016e-attempt1/runner/case-04/`；`host.stage` SHA256 `e6d9ec096640d5f9108231188f472b4c30ffe5e4cff3a8cb070030fe2386d6fe`，`test.stdout` `7d44f64da3c4c2bfaa71ff37ded772dd89487db7d8eab5fe13e1dcfbfa01adec`，`test.stderr` `655ededbdf653042f390a77981cb73911a70b6c7c9e9e1db7bc0c560b34009a2`。

## 现场逐次结算表

按本次 `supervision_started` 与 `created_suspended` 顺序分段；角色按测试源码中的初始 control、六次 real_limit 及其各自 control、末尾 job-cpu 对应。PID 只标识本次阶段记录，不跨运行关联。各行“是”均指该段记录了 ProcessGuard 对原持有进程句柄 `process_wait=0`、同 Job `job_query=1` 且 `job_active=0`、stdin/stdout/stderr 三路 `*_io_settled`、对应 reaped、`run_handles_released` 和 `supervisor_platform_returned`。本表不是事后用数字 PID 重新打开对象。

| 序 | 本次角色 | PID | 原句柄 signal | Job 查询/active | 三路 I/O settled | 回收及协调器返回 |
|---:|---|---:|---|---|---|---|
|1|初始 control|59944|是|1 / 0|是|io_complete_reaped / 是|
|2|start-fuel pages1|78812|是|1 / 0|是|io_complete_reaped / 是|
|3|control|71940|是|1 / 0|是|io_complete_reaped / 是|
|4|run-fuel pages1|68368|是|1 / 0|是|io_complete_reaped / 是|
|5|control|78588|是|1 / 0|是|io_complete_reaped / 是|
|6|memory pages1|77644|是|1 / 0|是|io_complete_reaped / 是|
|7|control|76100|是|1 / 0|是|io_complete_reaped / 是|
|8|memory pages2|76052|是|1 / 0|是|io_complete_reaped / 是|
|9|control|38984|是|1 / 0|是|io_complete_reaped / 是|
|10|memory-declaration pages1|75984|是|1 / 0|是|io_complete_reaped / 是|
|11|control|78600|是|1 / 0|是|io_complete_reaped / 是|
|12|memory-declaration pages2|74064|是|1 / 0|是|io_complete_reaped / 是|
|13|control|61764|是|1 / 0|是|io_complete_reaped / 是|
|14|job-cpu|76252|是|1 / 0|是|failure_reaped / 是|

前六个资源例 stdout 分别为 WorkerRejected、WorkerRejected、WorkerRejected、成功、WorkerRejected、成功。job-cpu 先在 `Expected::CpuLimit` 比较处报 `Err(Deadline)`，测试在 `test_observer::take()` 和后续 probe/通知断言之前 panic；不能说这些断言在测试中通过。但上述现有 host stage 对本次原持有对象的 wait/Job/I/O 收束有证据，原 test 对象也被 runner 观察到 exit101；因此可限定描述为**结果类别断言失败，目标 worker/Job/I/O 的收束已有日志证据**。不把此限定观察扩成全部可能外部对象的证明，也不自行解除 runner 保守闩锁。

## 已知事实与未知原因

probe 的 job-cpu 模式启动 8 条无限忙循环线程；本次 probe stage 有8条实际 started 记录及 request EOF。生产代码设置并查询同一 Job 的 `JOB_OBJECT_LIMIT_PROCESS_TIME`、`PerProcessUserTimeLimit=30000000`（3秒用户态）、128MiB 内存和 active1；整体绝对 deadline 仍是5秒。本例 Job 查询的 `TotalUserTime=261718750` 个100ns tick，即26.171875秒 Job 聚合用户态时间；该计数在退出后的 Job 记账查询中取得，不是持原进程句柄的单进程即时 CPU 采样，不按核数折算，不据其单独证明何时跨线或 Job 内全部对象归属。

本次通知日志有目标 PID count6 和 pid0 count4，无 count2；本地 windows-sys 常量分别对应 NEW_PROCESS、ACTIVE_PROCESS_ZERO 和 END_OF_PROCESS_TIME。测试的 JobEvents 在 Drop 时排队读取，普通完成端口消息并不保证送达，也缺消息发生时的时间锚；无 count2 不能证明内核没有执行时间检查。微软文档写明每进程用户时间限额由系统周期检查，而非承诺3秒实时硬中断；普通 Job 完成端口消息除规定例外并不保证送达。[Job 限额结构](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)、[Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)。故旧测试要求内核 CPU 退出和通知2一定先于5秒，缺少文档给出的时序保证；实际为何未触发，现有现场不能定论。

## 供经理裁决的最小候选（未实施）

在已有 `platform::run` 创建并持 `ProcessGuard.process` 的真实 worker 对象后、`io_loop` 内现有有界协调循环中，按单调时钟限频调用 `GetProcessTimes`，只读取同一原 process handle 的用户态累计值（API返回100ns单位，线程用户时间为系统累计；不按线程或核数除）。首次在 resume 后进入循环即采样，之后例如每10–20ms限频，并在成功接受前对已退出/即将接受的同一对象再采一次；CPU >= `30000000` 时关闭 Gate、复用原 Stopper/ProcessGuard、overlapped I/O 收割、原对象 signal 与 Job active0 流程，回收确认后返回独立的 Host CPU 预算错误，例如现有 `IsolationUnavailable("worker_cpu_budget", None)`；保留 Job 本来的3秒限额、5秒绝对 deadline、2秒回收和 cleanup_unknown/占槽，不加线程或公共配置。具体错误枚举/类型兼容由经理裁决。

CPU 查询失败必须捕获真实 Win32 错误并 fail closed，经同样收束后返回独立 `worker_cpu_query`；不能将读取失败或被 cancellation/deadline 抢先当预算触发。调用方返回顺序需明确：已有 biased select 中取消/Deadline 可能先于后台 CPU 结果，只有实录原对象用户时间跨线且 Host 路径实际赢得返回门才报告 Host CPU 预算；若系统 Job 先退出则保留真实 worker_exit/Job 证据，通知只记录是否收到，不人工合成或要求必须到达。响应刚好完成或进程已 signal 的竞态要在成功提交前的最终采样关闭，不能放行已知超预算的响应。

这是轮询、不是内核硬实时上限：2ms 循环/建议采样间隔、调度延迟以及任何卡住的系统调用都可能造成超过3秒后才观察到；CPU 采样由现有 supervisor 进行，不能保证协调线程卡住时及时处理。5秒 Stopper 仍是独立兜底。尚需审查 IO 前后/worker 退出和 cancellation 的 gate 仲裁，避免响应接受后才发现超预算或将 CPU 路径伪装为 OS Job 限额。

单因素动态建议（固定后另签窗）：以真实原进程 handle 保留创建时间、活性及 GetProcessTimes 采样序列、Host 跨线时刻与真实终止/回收；忙循环仅在前置证明该次环境能在5秒内达到3秒用户 CPU 时作为 Host 预算正例，否则记未覆盖而非放宽截止或将 Deadline 当通过。配相同装配的低 CPU 真实 W1 成功正控、测试态仅控制采样失败的 fail-closed 反例、取消与 Deadline 独立先胜/清理复用、完成/退出跨线竞态；每例均核原对象 signal、Job0、三路收割和占槽释放。OS Job 限额证据作为独立观测：若系统先杀且有对应通知/原对象退出可记录；无通知不等于未发生，更不能填造成 Host 成功。测试 fixture 不能替代本次失败原因。

本轮没有实施代码或改变原锁；需经理裁决错误语义、采样时机及该单因素验证范围后才继续实现。
