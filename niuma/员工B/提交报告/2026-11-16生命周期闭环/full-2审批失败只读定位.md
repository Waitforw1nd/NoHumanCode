# B：full-2审批失败只读定位

2026-11-16。只读经理`integrated-full-2/06-test.log`及其整合源码；未执行测试、未改产品或C独占的approval_gate测试。

窗口事实：lib 437 passed / 0 failed / 44 ignored；main runtime teardown 2/2、agent_runtime_cli 2/2通过；approval_gate 28 passed / 7 failed。全套仍未通过，5a9e57a5为窗口后待整合夹具增量，不据此反推已受测。

六项失败（日志对应测试行547、649、925、1111、1287、1645）在`setup()`完成`Engine::new`之后调用`e.store.recover()`。此时同域controller确实存活，`ExecutionAuthority::claim`在恢复写事务之前拒绝，符合新维护排他契约。C应迁移恢复前置，保留原重启幂等、审批顺序和文件/事件断言；B不放宽核心。

`ap15_real_command_effect_then_runtime_stop_unknown`行1041另有真实收束交界：旧runtime在真实命令写counter后shutdown_timeout，随即Store::open/recover。A `Operation::Drop`仅请求停止，独立holder继续持原generation/域，直到process/Job/readers实际收束，或决定unknown。日志busy本身不能区分旧controller尚存、票未释放或sticky unknown，也不能证明核心泄漏。

已同步C：这一项必须补有界旧controller/holder真实收束观察和失败诊断，保持counter恰一次、claimed恢复unknown及无重发；不能固定sleep、把超时视为成功或放宽claim。若观察到实际对象已证明全部结束而原票/controller仍不能释放，再交具体证据定位核心。当前七个报错位置未见必须修改核心的证据。
