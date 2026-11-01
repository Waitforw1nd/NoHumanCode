# NEXT-21W2 W04 attempt5 资源 CPU 截止失败与夹具增量

2026-11-01，员工 B。独立树 `../nhc-b-wasm-worker-process` / `codex/b/wasm-worker-process`；受测 `2d14e14f61112f1a51f1d0a8101a65f746e22219`、产品 `ae6c70c035b241621b4519600fa00e303d179d9a`、Rust `5a612320aea734bbe318917b928be66f337142c7`，runner PID 48452，现场 `NoManCode/rust-app/.local/next21w2-b-2d14e14f-attempt5/`。check、lib no-run、双工件 build、四 gate 与 W01 均 exit 0；资源组 exit 101 后首败停止，后续 W04、W02/W03、fmt/Clippy 未执行。

W01 before/after PID 60964/36272 均正常回收。资源组 start/run fuel、运行期 memory.grow max1 拒绝/max2 成功、同 max2 模块 manifest1 拒绝/manifest2 成功的六子例输出符合预期；这些部分不代表 W04 组通过。job-cpu PID 64048 到达 request_eof/cpu_loop_start，但 Host 五秒 Deadline 先到；Job 真实用户时间 10,000,000 个 100ns 单位，限额 30,000,000，IOCP 无过程 CPU 限额消息2。不能称 Job CPU 限额已触发。真实 process signal、Job query 成功/active0、三路 I/O settled 和 run_handles_released 均有记录，无 cleanup_unknown；终止与取消 API 返回不作为单独回收证明。

受测源码前后清单一致 SHA-256 `3c207d0c466e370ecf2ae595715477a19ce8b108e3589b4f1262fde98328fc61`；worker 快照 `d6502ccccb9ad73c22d195f348d79b69d5d486bbb22b13c33beb8932c0f75f8a`、probe `89bb64224b5c2eeb9dc8b7a242a01d42facbccb386744b03b8a8e0cba3333218`，110 项固定工件/模式副本结束摘要无不匹配。预检/结束树 clean，结束 heavy 空。经理独立核验详见 [首次动态审查](../../审查记录/第二十一轮/2026-11-01W04首次动态CPU截止失败.md)。

只读诊断时机器报告 24 物理核/32 逻辑核，即时总负载约 0.85%；该采样晚于失败，不能推出失败时的实际分配。临时诊断进程所在 Job 的 CPU rate 查询 flags=0，进程和系统 affinity 均 32 位全开；不能以此断言测试 worker 的所有祖先 Job 无其他限制。为提高同一进程用户态 CPU 累计，授权内将 probe 改为有限 8 个进程内忙线程并要求逐线程启动标记；未调整生产 3 秒 Job/5 秒 Host/2 秒清理限额、IOCP 消息或退出和回收断言。

新候选 `d8161a487c175fc3b548cc9daf79984a42dfbb41`，产品 `6a476ade46375b7ba6f18ffddfec5fb33e50725e`、Rust `c4f7203b822bdcce432cbd9a1edeba13413a2bb2`，fmt/diff-check 通过，经理核增量后新 attempt6 按原窗口执行；该新结果以其原始日志及后续报告为准。W04/W2、W05/W06、W3、A/P08 均未因 attempt5 通过。
