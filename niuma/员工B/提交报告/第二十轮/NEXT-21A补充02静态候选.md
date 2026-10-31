# NEXT-21A 补充02静态候选

2026-11-01，员工 B。基线 `ce1d721728a8eafbc7d762654dd748ff98e4eea5`；候选 `bcd6d9b089da9fe922fd82d4fb5d42bd89cefa44`，产品树 `06e883b7481ba8736f93ececf9f7123d35f1f53d`，Rust 树 `e77ec06582786266940f813f05f8c8de06abcfae`。独立树 clean；仅 `src/external_process_sandbox.rs`、`tests/external_process_isolation.rs` 变化，fmt 与 diff-check 通过，尚未编译或执行。

实际挂起进程 token 检查原 AppContainer、SID、能力、完整性、Job 后复制为 impersonation token，调用同一 AccessCheck 鉴别。三个独立 SDDL 共用 `O:SYG:SYD:P(A;;0x00000001;;;<实际TokenUser SID>)`，分别再追加 `(A;;0x00000001;;;<实际容器SID>)`、`(A;;0x00000001;;;S-1-15-2-1)`、不追加；具体访问位为1。记录描述符、请求位、各调用结果、即时错误码、AccessStatus 和 GrantedAccess；精确SID正控不允许、无授权控允许、API失败或异常 GrantedAccess 均关闭。生产要求 AAP 拒绝，`less_privileged` 为实际访问语义分类，Observation 标明方法；不再查询 class46，也不将旧87解读为真假。

新隐藏受控诊断入口使用同一创建和鉴别代码，只省略LPAC策略属性，生成实际普通 AppContainer 挂起进程，检查后结束 Job 并确认进程已信号、Job 活跃数0，绝不 resume；生产 `run_probe` 固定LPAC、无普通fallback。测试要求普通AC精确SID和AAP均允许、无授权拒绝、分类非LPAC且无resume。受控入口不证明完整隔离，描述符与系统实现尚待经理静审和动态正负控。

待经理审查后依补充02执行新窗口：workspace all-targets check、显式 probe build、新普通 AC 负控、原四项基础用例，前项全部成功后才 fmt check/Clippy，首失败停止并保留现场。实际 S01–S08 均不能据静态候选称通过；先前同步初始化硬截止和完整强杀、网络、资源、失败注入、工件等缺口仍在。
