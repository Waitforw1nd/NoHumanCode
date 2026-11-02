# B23 27c7 PRECHECK attempt1：check编译失败

2026-11-02，员工B；依经理[唯一签窗](../../审查记录/第二十三轮/2026-11-02B23-27c7-PRECHECK唯一窗口.md)运行。固定27c7 / 产品012665bb46ee4afffb446dc74e3d7761320434cd / Rust afa0765e16c10b70f56175597a09908fef8db6b9；原始现场在员工树Rust `.local/b23-precheck-27c7f601-attempt1/`，不清理不覆盖。

- exec session 31189已exit1；原supervisor PID 9304 / creation FILETIME 134380641191041516，原handle signal时间 2026-11-02T03:37:25.6247656Z，exit1。
- `cargo check --workspace --all-targets --offline --locked --target-dir <attempt/target>` 原PID 80468 / creation FILETIME 134380641448650099，2026-11-02T03:35:44.8570801Z 至 03:37:23.5253071Z，原handle signal、exit101。`src/process_lifetime_tests.rs:279` E0618：先前 `let Outcome::Settled(result)` 局部绑定遮蔽同名 `result()` 函数。原stderr SHA256 `cc71459d82e39cf80dfa8f60547a5bf6d799d7a4b0fa1e2247bdabbbe7c6ba61`；`01-check.json` `cf05352a0d0f23f956bb2c0e91fa88a8bff5372dab2d92d9fcbc91a24d94b27c`。
- 依首败停止；`02-clippy.json` 不存在，Clippy未执行；未运行测试、probe、Host或worker。此失败不是产品进程寿命行为证据。
- before、check前、check后、stopped四份源快照各196项，文件SHA256均 `527b6f76b10e519f6851677d6f03f2e90ce7daacee36b4ee70442c01a59ad295`；stop三对象不变、clean/index空。`artifacts-after-01-check.json` SHA256 `0a9f994b69c9cbc6e817bad5ea3ee628540bbbde8b6879f0f0b22ee17f2b88b7`。实际环境记录显示指定MSVC Hostx86/x64、SDK、jobs2/debug0/incremental0及E盘target/tmp/CARGO_HOME。
- common Git目录的 `nhc-heavy-window.lock` 仍存在，SHA256 `8b49909f99848df64757b3b56087d566dea013b8fa687a2d26b05afc38b3a96b`；`launcher.json` SHA256 `4058cff02e78790027a3f13ab8dec347a668da4a397dbb39d33789c8e27e89ba`，`environment.json` `0a0339f031ee157ec96707565d107fea38b795eaae1787ac8c9db5acf19c37cc`。本窗为确定失败、监督器已退出；不将Cargo直接进程退出扩大解释为后代树证明。锁的处置须经理核证另授权。

完整stderr另外报告 `peachsh` lib共43条 `dead_code`，全在尚未C接线的 `process_lifetime.rs`；逐条对象为：`DomainLease`、`OverflowPolicy`、`ProcessSpec`及`new`、`Capture`、`CompletionReason`、`Settled`、`ProcessIdentity`、`Outcome`、`NotStarted`、`Phase`、`LeaseState`、`LeaseCell`及`decide_unknown/finish/take_outcome`、`Capacity/reap_completed`、`Slot/release_unstarted/attach_current_thread`、`Supervisor/new/resize/reserve/submit`、`Operation/request_cancel/cancel/wait`、`Signal`、`notify`、`unknown`、`WatchdogExit`、`raw`、`last_error`、`owned`、`Holder/finish`、`Evidence`、`Attributes/new/set`、`wide`、`append_argument`、`environment`、`pipe`、`reader`、`accounting`、`prestart`、`run`、`run_inner`。完整原文、行号、诊断上下文保存在上述原stderr；没有声称其余尚未到达的编译/lint问题不存在。

按已授权范围更名该局部变量消除遮蔽，并只在`lib.rs`中对`process_lifetime`模块的非test编译添加标明C生产接线后移除的临时`cfg_attr(not(test), allow(dead_code))`；test编译继续报告实际警告，没有扩大API或关闭整crate lint。以新正常提交提出候选；不自行清锁、不重启或建立新attempt，新的编译结果须另签。
