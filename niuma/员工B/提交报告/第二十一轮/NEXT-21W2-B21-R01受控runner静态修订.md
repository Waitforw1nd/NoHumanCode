# B21-R01 受控 runner 静态修订

2026-11-01，员工 B；同一第二十一轮 W2 任务。经理静审 f89aed73 主体测试可进编译准备，但 runner 同步等待终态 unknown 为 P1。C 持动态窗口，本次无构建和任何测试进程运行。

相对 `f89aed73c79696729469a2ea5d80cf14fe2a1553` 的增量提交 `e544db0cc82433512e4bcb23345202e8ab22fa5c`；产品 tree `2e0ac6142f503bf53f35da729bb4c0cecaf3ffc9`，Rust tree `dcd55e88295babe42fee0e28c2055def2ce268c7`。分支 `codex/b/wasm-worker-process`；提交后独立树及 index 均空，主仓 index 未操作。

增量只涉及 `NoManCode/rust-app/tests/support/wasm_worker_process/run-controlled.ps1`、新增同目录 `DirectProcess.cs`，及 `src/wasm_worker_process.rs`、`src/wasm_worker_process_tests.rs` 中测试态终态标记。未修改生产隔离行为或 W04 对预期暂时 unknown 的断言。

runner 通过 Win32 在挂起状态创建固定 lib test，先持真实内核进程句柄、取得 PID/创建 FILETIME，再核同一 .NET Process 对象并恢复；标准输入为 NUL，标准输出/错误是直接继承的现场文件句柄，避免依赖 runner 管道读取线程。以 Stopwatch 和 HasExited 轮询，每例默认 180 秒（允许 30–1800），检测专用 `NHC_TERMINAL_UNKNOWN:` 或真实创建句柄终态 unknown；不匹配一般 `unknown`，W04 已预期并完成回收的单例可正常结束。每例写开始、nonce、测试名、父测试进程 PID/creation time、固定工件 hash、日志路径、结束或事故时间和状态。枚举测试也走相同受控启动和期限。

终态标记、超时、启动后观察失败、非零退出或不能确认单例通过均停止后续；保守标为回收未独立确认，写 attempt `incident.json` 和现场 `case.json`，并保留源码树 `.local/nhc-w2-window.lock`。后续本 runner 跨 attempt 使用 CreateNew 拒绝开窗；该闩锁不替代人员管理其他启动方式。runner 不 kill、按 PID 猜杀或声称回收，进程仍保有自己的对象；runner 返回后其观察句柄随进程结束而关闭，现场中的 PID 与创建时间用于后续受控核对，不能把 PID 当对象身份。未确认解除前不得重新开窗。系统调用或文件系统本身不可保证一定返回；本次没有动态证明超时行为。

必须传 `-ExpectedHead` 完整 SHA：首次 cargo 前核 HEAD、产品/Rust tree、工作树及 index；保存 source-before，构建后复核保存 source-after-build，每例前核，结束及退出复核保存 source-after/source-final。固定 worker/probe/lib test 及逐例副本的 hash 检查保留。实际签窗入口（在 Rust 目录、新 PowerShell 进程，路径为实际字符串）：

```powershell
.\tests\support\wasm_worker_process\run-controlled.ps1 -ExpectedHead e544db0cc82433512e4bcb23345202e8ab22fa5c -TargetDir <独立target目录> -AttemptDir <不存在的attempt目录>
```

静态 PowerShell parser、两份 Rust 的 rustfmt --check、提交前后 diff --check 通过。DirectProcess.cs 仅静态阅读，未编译；未执行 cargo、Add-Type、runner、lib test、helper/worker/probe、正式测试或 Clippy。该增量待经理集中 review 与授权窗口验证；不能据静态检查宣称动态成功。
