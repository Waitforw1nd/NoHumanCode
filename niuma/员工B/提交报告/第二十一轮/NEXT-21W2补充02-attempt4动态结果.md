# NEXT-21W2 补充02 W02/W03 attempt4 动态结果

2026-11-01，员工 B；依经理 [动态窗口](../../审查记录/第二十一轮/2026-11-01W2协议管道固定候选与动态窗口.md) 执行。独立树 `../nhc-b-wasm-worker-process` / `codex/b/wasm-worker-process`，基线 `8937aed18124c4db75c115b9849797b49fd3c51d`，固定受测 `ba4f72cf70c75d25880a63fdb50538204c37d762`、产品树 `5e7eab49deeeb1de1f979e2c783e89c99bea46bd`、Rust 树 `1245c1592ff237c8cf5b978fc921b0f1f43e31e5`。B 独立树执行提交和测试，经理负责整合；主仓人员档案未由 B 暂存或提交。

## 执行结果

现场 `NoManCode/rust-app/.local/next21w2-b-ba4f72cf-attempt4/`，runner PID 9036，预检身份、clean 和重型进程空。依次 workspace all-targets check、lib no-run、同次真实 worker 与 probe build、真实 W01、W02 exact、W03 exact、fmt check、workspace all-targets Clippy `-D warnings` 均 exit 0。各步命令、时间和退出码保存在 `logs/01-check.json` 至 `logs/08-clippy.json`；exact 均单线程。四 gate 本窗口按经理指示未重复运行。

真实 W01 before PID 50424、8,388,544 字节模块；after PID 48536；两次 exit 0、实际 process signaled、Job active 0、I/O completed。W02 共 33 故障模式，W03 共 10 模式，测试各自断言目标类别、相同 PID 阶段、进程观察句柄 signal、Job 查询与 active0、三路 I/O 终态、句柄作用域释放及下一次正控复用；每例 PID 和结果见两个 stdout 原件，详细阶段在各独立 stage 日志。没有出现 cleanup_unknown，结束 heavy 空、源码树 clean。

W03 部分读 PID 62492：probe 记录 read_32、pending_confirmed、stdin_closed，Host 返回 `IsolationUnavailable("io_completion", Some(109))`。完全不读、部分 stdout、关闭 stdout 及并发例按约到 Deadline 并完成回收；两种输出超限各自命中指定错误。

真实 stdout 和 stderr `WriteFile(0)` 与后续字节各已执行；两例均打印 `actual_zero_read_observed=false`。stdout 尾随被拒绝，stderr 后续内容被读，**但不能宣称成功零读分支获得自然动态覆盖**。迟发例在等待期间被截止清理，未证明 worker 在截止后实际发出数据。

双路压力 PID 47568：stdin pending 时累计已写 16,384；`GetNamedPipeInfo` 两路各报 16,384 字节额度，而 `PeekNamedPipe` 两路各报 131,072 字节可读。之后放读，stderr 累计到 81,920、命中 stderr_limit，三路终态与 process/Job 回收均有记录。两写线程均记录尝试，但**没有独立观测证明放读瞬间两次写系统调用都仍阻塞**；可读字节大于报告额度，不能只据额度认定双写阻塞。请经理裁决这一证据覆盖范围。

## 工件与清单

构建后立即复制独立 worker 快照 SHA-256 `f954a5d48e152366277991fe4e9e16417a946e43272a1e64ae4a96da747c38f0`，probe 快照 `d919a03da807da481061eda955d9436c33a8694ea9808bf7b05e8c2fc4dbfdad`。W02/W03 各 44 个独立模式副本，逐个记录并校验 SHA；结束复核两快照及 88 副本共 90 项，零不匹配，原件、模式清单及 `logs/verification.json` 保留。189 个源码条目清单前后文件 SHA-256 同为 `8e81834a739c73871dfef792987b3248a2631e80f935d7e7ba9ef66ebbb85d96`；旧 attempt2/3 不变。

本报告交经理核查，不等同完整 W2 或产品接入验收。W04 交界/未知注入、W05 真实 Host 强杀、W06 工件/继承/非 Windows、安装信任根、W3 持久恢复和 A/P08 真实 profile 可写反例仍开放。下一步经理审查 attempt4 原始证据并裁决双路阻塞与零读未命中范围。
