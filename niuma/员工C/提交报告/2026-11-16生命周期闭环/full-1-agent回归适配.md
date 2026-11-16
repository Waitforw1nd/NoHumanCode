# C：full-1 agent crash/admission 适配

2026-11-16，经理新授权 `src/agent_runtime_tests.rs` 唯一写权由 B 转 C。仅修改该测试文件；不改固定 integration HEAD，不改 D 的 provider_runtime_tests.rs。全 U 映射暂缓。

经理执行的 integrated-full-1/06-test.log 显示5个 crash 测试在 ready_task_identity 前 child exit101，另1个 admission barrier 超时。只读 `.local/lifecycle/integrated-full-1/tmp/.tmpgpApjT/crash-child.stderr.log` 确认子例 Store::open 报 data domain instance is busy。C与固定integration该源文件原blob同为62b310c99786ab3f54ed902a7b43d438bd34966e。

修正：Fixture 保留 mock/server/目录，Engine 改为可取出的持有包装。5个父例启动child前先有界shutdown、断言settled、drop Engine，再以原Store的Weak确认无剩余强引用。未创建替代owner、未放宽生产域锁。

两个需要跨进程审批的用例不再从父Engine访问活child的域。child使用自己的Store发布原审批与任务快照；parent核task绑定和脱敏后原子发布approve/deny/cancel决定，child沿原Engine/Store API处理。cancel_pending_approvals恰1条断言迁至child，取消/拒绝后的持久状态与原result/event断言仍在parent。两端15/10秒界限；原child创建对象kill/wait、崩溃停点和证据目录保持。所有本文件测试child启动统一隐藏窗口及test-threads=1，exact名称不变。

3个n19恢复例在第二次重开前补shutdown settled，避免仅drop调用者时后台结算仍持controller。n18 barrier key从原Store实际SQLite连接读取，匹配生产canonical文件名；原测试另开裸SQLite连接会使用不同路径表示，这是静态识别的匹配问题，待经理复测。

|保留的exact名称（均带agent_runtime_tests::前缀）|改动与保留断言|
|---|---|
|a10_real_kill_recover_resume_never_replays_prior_tool_group|父释放；4个真实崩溃停点、2次恢复、tool顺序/预算/调用次数/无重复写全部保留|
|a10_unknown_approved_write_survives_restarts_without_automatic_retry|父释放、child执行父审批；claimed后原对象终止、2次unknown恢复、无重发/文件未写保持|
|n18_agent_admission_and_remote_disable_have_a_transaction_order|实际Store连接barrier key；先准入/先停用两种事务顺序、lease计数及拒新保持|
|n19_redacted_decision_process_kill_reconciles_once|父释放、child处理deny/cancel、任务快照脱敏；持久审批事实、无tool_start、唯一result、新turn及二次恢复保持|
|n19_legacy_read_event_child_kill_then_reconcile_and_new_turn|父释放；legacy事件原值、唯一结果、无重发、新turn、二次恢复保持|
|n19_builtin_host_denied_after_commit_kill_recover_and_new_turn|父释放；原HostDenied结果/投影/事件、无审批和workspace change、新turn、二次恢复保持|

仅执行单文件rustfmt及git diff --check；未运行Cargo、测试或真实进程。编译与行为待经理下一check窗口。此前C69226513+Bbc291abe定向18/18及D4/4通过仅引用经理执行回执，不是C执行结果；旧失败原件保留。
