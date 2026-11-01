# B21-W2-FULL-A 第二次窗口停窗与取证增量

2026-11-01，员工B。经理授权固定 `249479ad1f42263c12dccf7d5d4b467c1331319f`，产品 `7889e6e7c2686c47dcb1c7a7a9a167cfa11756c5`、Rust `fb61c035b923b2d883c760efb86489c7509a01e4`；受测前、构建后、退出时 source 清单均记录同三项及工作树/index 空。UTC 15:42:08.4139911Z–15:43:22.4461431Z。现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-a-249479ad-attempt2/`，E 盘 target/TMP/TEMP/输出，offline/locked、jobs2、debug0、incremental0；复用本人独立旧 target。外层隐藏 PowerShell，轮询主窗口句柄为0；固定 DirectProcess 本轮调用 flags `0x08000004`。没有对所有瞬时窗口作完整观测。

执行 `run-controlled.ps1 -ExpectedHead 249479ad1f42263c12dccf7d5d4b467c1331319f -TargetDir <B旧独立target> -AttemptDir <全新runner现场> -CaseTimeoutSeconds 180`；原始实际参数和 MSVC/SDK 见 `environment.json`、`start-window.ps1`。Add-Type 成功；build、lib no-run 分别越过非零门禁、日志 Finished（脚本没有分开保存具体退出码，按控制流为0）；固定三件后 `__list__` exit 0，核 23 名称/20 exact + 3 helper 的映射。前三件源/固定副本的构建后和停窗后 SHA256 均一致：worker `2fc364dfe8ad85d709a8deced561985c4a2b7db92231d5c17ff806c5c2c49c5c`，probe `cacd1822ab43ed45f587daf95d5ac0936a47dce083e7902f5342f1453c5a9ed2`，lib test `719a29347a8853d60d88ff1e676a79239ec31022d86aa6756b6242ffa1318bfe`。前5例 runner 已逐例校验工作副本；第6例事后 worker 与53份 probe 副本 hash 复核一致（`runner/case-06-hashes-after.json`）。

每个已运行 exact 的原始 stdout/stderr、命令身份、UTC、PID/creation、exit 与场景文件在 `runner/case-01` 至 `case-06`：

1. W01 `w01_real_worker_before_after_and_near_8mib` exit 0，1 passed；真实 worker 两次成功，PID 72516/70508，均 signal、Job active0、I/O完成；before 模块 8,388,544 字节。
2. W02 `w02_protocol_fault_matrix` exit 0，1 passed。
3. W03 `w03_pipe_fault_matrix` exit 0，1 passed。
4. W04 `w04_resource_limits` exit 0，1 passed。
5. W04 `w04_injected_zero_read_sensitivity` exit 0，1 passed。
6. W04 `w04_unknown_cleanup_and_slot_reuse` exit 101，0 passed/1 failed，`src/wasm_worker_process_tests.rs:1976` 报 `created worker was observed`；其余14 exact、W05/W06、fmt、Clippy、额外回归均未启动。

第6例 runner 持有原 test 进程对象并记录 exit 101；PID 77680、creation FILETIME `134380213993600100`、nonce `ebabddf6f5ee44d7b53aea0f761abd7c`。前置 control 的 stage 记录本次 PID 77696 的 `process_wait=0`、`job_active=0`、I/O reaped 和 handles released。注入分支实录 `create_ticket_granted → create_after_ticket → stopper_cancel_io_return → cleanup_unknown → injection_released → create_flags → create_failed`，没有该分支 `created_suspended` 或 PID；日志当时没有 Win32 创建错误码，也没有 Stopper 终止 Job 的返回值或该分支 Job active/total，不能判定错误原因、不能以 PID 缺失断言安全或将其当 late Create 成功。当前 PID/子进程数值快照空，仅作现场观察、不反证历史。runner 将非零例保守记 terminal_unknown；`nhc-w2-window.lock` 保留，SHA256 `69be14f47b5914c6595f9fd032f4805b2f2119054b540859833a0051f4eeb14b`。未杀、重跑、删除或归档该锁；经理核终态前不得开窗。

经理要求只读取证与静态修：固定增量 `44689e1970d7064d2c1c65091c29f773ef6e79e6`，产品 tree `20fa7d20275d2bb532966b5629dfa60d9956d39b`、Rust tree `79776100435538b70099ce5891b693e30f0ed058`，相对受测候选仅 `src/wasm_worker_process.rs`。测试态记录 CreateProcessW 即时 Win32 错误、Stopper 首次 TerminateJobObject 返回及错误、Stopper join、同一 Job 在 Stopper join 后的查询/active/total、platform 返回位置；slot 释放继续依据原实现。失败分支的测试态主动 join 后再查询 Job，改变测试观测时序；生产路径不增加该查询。原 `created worker was observed` 断言保留，成功路径仍须原句柄 signal；未在未知错误码时将失败当通过，`create_returned_suspended` 等后续阶段保持。该增量仅 rustfmt --check/diff-check，未编译或运行；不能推断已确认根因。独立树/index 空。

按用户授权，已将首个 attempt 与 audit 的必要日志、JSON、脚本 25 件逐字节发布到本报告旁 `B21-FULL-A-attempt1证据/`，不包含大 EXE/target；`双hash清单.json` 记源相对路径和源/发布 SHA256，`发布复核.json` 显示0差异。清单 SHA256 `eaf1eec7299a587d4a2f0c662d2d8e34094e05e695661c191de8f9cd32d85475`；历史日志内绝对路径未改。当前窗口首败已归还；下一步经理核事故对象与闩锁、审取证增量，未经新授权不启动动态。
