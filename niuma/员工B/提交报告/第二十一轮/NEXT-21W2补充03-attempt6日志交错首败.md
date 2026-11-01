# NEXT-21W2 W04 attempt6 成员关系日志交错首败

2026-11-01，员工 B。独立树 `../nhc-b-wasm-worker-process`，受测 `d8161a487c175fc3b548cc9daf79984a42dfbb41`、产品 `6a476ade46375b7ba6f18ffddfec5fb33e50725e`、Rust `c4f7203b822bdcce432cbd9a1edeba13413a2bb2`；runner PID 64300，现场 `NoManCode/rust-app/.local/next21w2-b-d8161a48-attempt6/`。check、lib no-run、双工件 build、四 gate、W01、资源、R06 注入、取消/接受交界、七种预期 unknown 释放回收、两种持续观察错误组 exit 0。成员关系组 exit 101 首败停止；R04 实际不同 Job 反例、W02/W03、fmt/Clippy 未执行。

资源组真实 WAT 六子例满足约定；job-cpu PID 57232 实得 Job 限额通知2、exit -1073741756，约 3.449 秒墙钟，Job 用户时间 49,218,750 个 100ns 单位（约 4.92 秒），配置限额 30,000,000；本次调度和记账超过配置值，**不把 3 秒表述为绝对无超额硬实时上界**。job-memory PID 46744 通知9、exit43，Job 峰值 131,022,848 字节。R06 两次是明确注入，不能改写 attempt4 自然零读未观察到的事实。

首败 PID 2696 实际 membership=1，注入分支后 job/process 终止记录行与 stopper 记录交错，使 `terminate_process_return` 行格式损坏，断言未找到完整行；原始文件保留，不能把损坏行当完整的调用返回。该 PID 的实际进程 signal、Job query1/active0、failure_reaped 与 run_handles_released 存在，未出现未预期 cleanup_unknown；结束树 clean、heavy 空。两个 unknown 组的预期未知均在放行后完成实际回收并正控复用。

前后源码清单 SHA-256 同为 `71ac1abf0d461569d73c72a65efebecf2da9c7c7a681cb61cbf3be073db8e3a3`；worker 快照 `d6502ccccb9ad73c22d195f348d79b69d5d486bbb22b13c33beb8932c0f75f8a`，probe `05c336082a5032ffdb26e08ad34ffacfde3efeea54cd0bac6ef1f8aec0b149a3`，两工件与模式副本共110项结束摘要无不匹配。

授权内新提交 `dbba89e90736696f1a9bc0bf4520495f260b1519`、产品 `4c9b80eff52d0f338297717b51d8d767d9cf81ad`、Rust `bee7ae8505d9878106228d00edb2cf1bd6282090`，单一测试态 Mutex 串行每条日志及测试标记，锁不跨业务 OS 调用/停点/await，不删断言；核查错误码仍在诊断前保存。fmt/diff-check 通过，经理已核增量，attempt7 独立运行；新结果不得反写 attempt6。W04/W2、W05/W06/W3 与 A/P08 仍未验收。
