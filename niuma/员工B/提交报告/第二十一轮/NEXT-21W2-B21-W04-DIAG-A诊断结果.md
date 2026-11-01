# B21-W04-DIAG-A 单例诊断结果

2026-11-01，员工B。经理签唯一诊断窗口，只运行 list 与 `wasm_worker_process_tests::fault_matrix::w04_unknown_cleanup_and_slot_reuse`，不运行其他 exact。固定源码 HEAD `796f8847eaf32bc2494cc2ad962f781f9ba19bfb`、产品 tree `d1dd57a190246254d476c2f94d1dea638ee91d5f`、Rust tree `24ecb5761c7acccf49ebd20c8ff9c622e0b980fe`；前、构建后、最终源码清单相符，工作树/index 空。窗口 UTC 15:53:32.1450051Z–15:54:30.7318938Z，独立现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w04-diag-a-796f-attempt1/`。

入口 `run-controlled.ps1 -ExpectedHead 796f8847eaf32bc2494cc2ad962f781f9ba19bfb -TargetDir <B独立旧target> -AttemptDir <本次新runner> -ExactTest wasm_worker_process_tests::fault_matrix::w04_unknown_cleanup_and_slot_reuse -CaseTimeoutSeconds 180`；实际完整路径/环境见现场 `start-window.ps1`、`environment.json`。E 盘 target/TMP/TEMP/日志；offline/locked、jobs2、debug0、incremental0；MSVC/SDK 沿项目设置。CIM 启动前两次返回“调用已取消”，改用 .NET 进程快照0个匹配，记录于 `preflight-processes.json`。外层隐藏 PowerShell MainWindowHandle=0；DirectProcess flags 为 `0x08000004`；没有对全部瞬时窗口作完整观测。

`selection.json` 明确 single/1/映射20；Add-Type 成功，worker/probe build meta 及 lib no-run meta 分别 exit 0；list 原对象 exit 0，20+3 全映射核对通过。目标 exact 原 test 对象 PID 46436、creation FILETIME `134380220527453540`、nonce `7165d66366f94ef885eeb9f6af11ea43`、exit 0，1 passed；窗口 exit 0。只代表该单例该次结果，不代表完整 W04/W01–W06 或 W2 通过；旧 attempt2 前5例只属于 249479，不能追认为本候选通过。

本次 `create_after_ticket` 在日志的具体顺序：`create_ticket_granted → create_after_ticket → stopper_terminate_job_return count=1 → stopper_cancel_io_return count=0 → cleanup_unknown → injection_released → create_flags 525324 → created_suspended PID77128 → process_wait count=0 → job_query count=1 → job_active count=0 → failure_reaped → stopper_joined → run_handles_released → supervisor_platform_returned`。测试 stdout 标示该停点 original process 已回收；没有该 PID 的 resumed 记录。此次 CreateProcess 成功返回挂起对象，没有执行新增的 `create_failed_win32_error` / `create_failed_job_query` 分支，因此没有得到创建失败错误码或失败分支 Job 计数；不可由此次成功推定旧 attempt2 的错误原因或证明失败分支。

其余六个测试内注入停点也在本次单例执行并报告实际对象回收：`create_returned_suspended` PID46480、`submit_after_ticket` 78608、`submit_returned_pending` 38420、`observation_injected` 78092、`result_observation_injected` 78084、`cancel_after_api` 70940；均以各自本次持有的进程对象 signal、Job active0 和后续回收日志为证，不能用跨运行 PID 建立身份。旧 case06 首败 `create_failed` 依然有效保留。runner 临时 cleanup_unknown 为预期在途状态，最终各分支有实际回收，不是 terminal_unknown；正常关闭闩锁。

本次 worker SHA256 `63eef9ce2895c6f73a5389a321d6ff2c0fe8bccbd47048afb0bd23c4be3b8956`、probe `b3cf1f0e3aa6c8d60d5a4a04c4ae2b3b3a20f91d2c0b8fd122e2df2c8a95d2f2`、lib-test `3024421fca421dfb7d09cc02c325aa378d032e55f52b28038ba6e2518f0f785f`；本次源/固定件前后相等，worker 现场副本与53个 probe 副本事后核一致，详 `runner/hashes-after-window.json`。本次重新构建的 hash 与旧 249479 的三件不同，未借用旧产物证明新版本。

下一最小取舍建议：保留 late-success 分支现有原 process handle signal/Job0 与 `create_returned_suspended` 必须成功覆盖；给 `create_after_ticket` 单独建立受控失败观测分支，只有真实 CreateProcess 返回0、即时错误码可用、Stopper 实际结束、同一 Job 查询成功且活动数0、平台实际返回和 slot 释放均有证据时，才将其记为“创建失败并完成回收”的独立结果；错误码和触发条件先经经理裁定，不能将任意 Create 失败等同安全通过，也不能从 PID 不存在推断。应保留有效控制与成功分支单独覆盖；若仅偶然竞态则需另有可复核的受控触发，不能靠重跑到绿色。此为建议，未在本窗口改代码或断言。

按授权将 attempt2、其锁归档 audit、本次诊断共143份必要日志/JSON/脚本逐字节发布到本报告旁 `B21-attempt2与W04诊断证据/`，排除 EXE/target/tmp；`双hash清单.json` 含源相对路径与源/发布 hash，清单 SHA256 `2484c1a1c0707a4381f1bf5c08b51109ed16790c5e0f749c352e5ceb92cc5927`，`发布复核.json` 为0差异；历史原文的绝对路径未重写。结束 .NET 快照无匹配重型进程，源码树 clean/index 空、gate absent；窗口归还，下一步经理核证及裁定。
