# B21-W2-FULL-B 首败停窗

2026-11-02，员工 B。经理签唯一完整窗口，受测 `82a0016ef1d582abc963270b48149e10fca1220f`、产品 `99ed0add4a89fbee1473a3c511a9972a54148a65`、Rust `8fccff04244c3a3b04e120f7bbb2b6e3d5ecdea4`；前、构建后及退出时源码状态相符、工作树/index 空。窗口 UTC `2026-11-01T16:08:58.9439888Z`–`2026-11-01T16:10:47.1134432Z`；新现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-b-82a0016e-attempt1/`。E 输出/复用本人独立 target/TMP/TEMP，offline/locked、jobs2、debug0、incremental0；MSVC/SDK 及实际路径见现场 `environment.json` 和 `start-window.ps1`。外层 PowerShell 隐藏启动，观测 MainWindowHandle=0；DirectProcess 创建 flags `0x08000004`，不声称覆盖全部瞬时窗口可见性。

实际入口为受控 `run-controlled.ps1 -ExpectedHead 82a0016ef1d582abc963270b48149e10fca1220f -TargetDir <B独立旧target> -AttemptDir <本次新现场> -CaseTimeoutSeconds 180`，默认 full。`selection.json` 为 full/20；build meta exit0，lib no-run meta exit0；list exit0、20+3 名称映射完整，之后串行进入 exact。各例 test 进程的真实 PID/创建 FILETIME、开始/结束 UTC、exit 和日志路径均在现场相应 `case.json`。

1. W01 真实 worker before/after/近8MiB：exit0、1 passed；case-01。
2. W02 协议矩阵：exit0、1 passed；case-02。
3. W03 管道矩阵：exit0、1 passed；case-03。
4. W04 `w04_resource_limits`：exit101、0 passed/1 failed；case-04。六项真实资源正反例已有输出，随后 `job-cpu` 断言期望 `IsolationUnavailable("worker_exit", Some(nonzero))`，实际 `Err(Deadline)`，失败位置 `wasm_worker_process_tests.rs:378`。

第4项原 test 进程 PID57692、creation FILETIME `134380230371094423`，nonce `8c255fd0fe69436db8e6018b68ceac87`；runner 持原对象记录 exit101。其 job-cpu probe 本次 PID76252，probe stage 有 started/request_eof/cpu_loop_start/8线程启动；host stage 后续记录取消 IO、TerminateJobObject 返回1、process_wait=0、job_query=1/active0、用户 CPU 100ns 值261718750、per-process limit 30000000、failure_reaped、Stopper joined、handles released 和 supervisor 返回。上述是本现场阶段记录，不推断哪个 OS 因素导致 Deadline，不用后续日志改写实际断言失败或把失败认定为预期 CPU 限额成功。

runner 按非零退出保守标记 terminal_unknown，保留原 `NoManCode/rust-app/.local/nhc-w2-window.lock`，SHA256 `a73a9675dff8b8009a83ee07d90d5902519c9c97c46069c6bc6e9ec964ce59e6`；事故文件 `runner/incident.json`。未对 PID 猜杀、未归档或删除锁、未重试；退出后 .NET 数字进程快照无匹配重型进程，仅为当时快照。第5–20项均未启动，因此新增 W04 真实 API 失败 fixture、W04 原正常成功停点、W05/W06 及其他剩余 exact 均无本轮结果；fmt、Clippy、回归与主线整合也未执行。

本轮源及固定副本前后 SHA256 一致：worker `f981aaabfb14534a36e1fed8607d50e37c6856a966a73aba7478a51a943cf443`，probe `f8b6521fe824928065f5d573c28a671e9b92b7b706c0b565fe7f835182e1a43c`，lib-test `a4833adccdf808918c0d252ee67810bfc68a69d9fac09ab616b8362f36300772`；失败例本地 worker 与53个 probe 副本事后 hash 复核一致，记录 `runner/hashes-after-window.json`。前三例 runner 在结束后已逐例校验；历史候选结果不追认为本候选通过。

窗口首败已归还；下一步经理核原 test 对象、job-cpu 原始阶段、限额证据及闩锁并裁决。未经新授权不启动动态、不更改原锁或现场；本轮不是 W2 验收或 main 合入。
