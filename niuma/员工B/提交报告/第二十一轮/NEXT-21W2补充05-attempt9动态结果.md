# NEXT-21W2 补充05 attempt9 最小动态结果

2026-11-01，员工 B。按经理 [无控制台修正验证窗口](../../审查记录/第二十一轮/2026-11-01无控制台修正候选与验证窗口.md) 执行。候选 `1d1c1fcb706edf01b87f230c43be221a1d818232`，产品树 `4684bd59691a4fb7b844f60575057c48edf8a634`，Rust 树 `ce1172cdbcee694b131985396f53e23ff9c1f185`；独立树 `../nhc-b-wasm-worker-process`，原始现场 `NoManCode/rust-app/.local/next21w2-b-1d1c1fcb-attempt9/`，旧 attempt 原件保留。

## 实际结果

六步均 exit 0：真实 worker + probe build、lib no-run、P01 单线程 exact、W01 单线程 exact、fmt check、workspace all-targets Clippy `-D warnings`。两项 exact 分别实际运行 1 例，均 0 failed。Clippy 另报告依赖 `proc-macro-error2 v2.0.1` 存在未来 Rust 不兼容提示，不影响本次 exit 0，但不忽略该提示。

- P01：实际创建 flags 为十进制 `525324`（`0x0008040c`）。目标 PID 9276 的创建时间 FILETIME `134379776452395036`；seq 12 在挂起目标锚上实测映像、活性和 Job 成员；scan 4 的 seq 15/16 两次列表仅目标，active 1、total 1，目标对象和目标锚前后均 wait 258 / 成员 1。probe 完整读取、进入等待并放行后真实内存分配失败，错误 1455、退出码 43；seq 22 同目标句柄 wait 0，Host 记录 Job active 0、I/O 收割、collector stopped、run handles released。seq 18 活跃阶段的再次映像查询失败 error 5，seq 22 退出后查询失败 error 31；两次成功的现场映像记录在 seq 12/15，失败未被改写。
- PID 数值 9276 恰与 attempt8 额外对象数字相同，二者创建时间和实际对象不同，不能关联或重新判定 attempt8。该次诊断未见额外对象，结论仅限实际采集的现场：完成端口通知、进程列表和各 API 并非原子，不证明其他时间或系统版本绝无短暂对象，也不替代 W06 单因素验证。
- W01：使用刚构建并固定的真实 W1 worker。before PID 47956、模块 8,388,544 字节，after PID 34980；均返回有效策略、exit 0、process signaled、Job active 0、I/O completed；两个调用的日志均记录 flags 525324、三路 I/O settle 和句柄释放。不是正式 Engine/CLI 已接入的证明。

## 快照与证据

原始 `logs/` 包含六步命令、退出码、完整 stdout/stderr、分别独立的 P01/W01 stage、源码前后清单、工件摘要和运行环境。P01 的 probe 原始阶段位于 `modes/p01/p01-memory.stage`；其实际执行 lib 测试文件与 no-run 同一路径及散列，W01 同样使用该测试工件。

probe / p01 副本 SHA-256 `3a0582a3c36a4db242fbdd8a0b16145d280e14fa63ad8bbb1e2d52bb1f92770d`；真实 worker `e83c3c4876722c2be6bd2635166625f2c2475a241bd025bb442bb52e90ca8d0a`；lib test `44d95c8d2c83aae871f5a6ab548794990076b2e0133c84bbe9932c473129398f`。工件源、固定副本和模式副本结束重新散列一致。189 条产品源码各步清单 SHA-256 均为 `5c1e044b7711a87f3c01fdf1b8afaf3d6aad2a082789ae7b4c4feacd8d53d8dd`。最后 Git 状态 clean、heavy 进程清单为空。

本窗口只覆盖最小创建 flags 修正及真实 W1 管道。W05 最后 Job 句柄强杀、W06 其余工件/继承/active-breakaway、其他受影响回归及正式生产绑定仍开放；W2 未合入主线，交经理核证与决定下一步。
