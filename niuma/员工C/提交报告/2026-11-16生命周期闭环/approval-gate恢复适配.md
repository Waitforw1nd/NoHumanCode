# approval_gate 恢复适配候选

仅修改 tests/approval_gate.rs。依据经理 integrated-full-2/06-test.log 的 28 passed / 7 failed；此结果由经理运行，C未运行测试。

六处 setup 后 Store::recover 改为 setup_recovered：在旧 runtime shutdown_timeout 已执行之后观察 admission=(false,0,0)，由 Engine::new_recovered 在唯一 controller 内恢复并激活。不改变原审批状态、事件次数、顺序、脱敏、权限、无重发断言。

ap15_real_command_effect_then_runtime_stop_unknown 保留原 runtime 骤停，不用 graceful Engine shutdown 改变业务结果。旧 Engine 的 Store observer 跨 runtime teardown 保留；最多12秒观察原 holder 的票实际归零且无unknown、gate关闭，随后确认审批仍Claimed，释放observer再恢复。unknown/超时写 restart-holder-unknown.json、输出 NHC_TERMINAL_UNKNOWN 并原进程停窗持有现场，绝不恢复/猜杀/继续下一case。既有counter一次、UnknownResult、无tool_result断言全部保留。

精确测试名称不变：
- ap06_ap07_ap18_runtime_restart_and_decide_before_waiter
- ap06_reconcile_preserves_order_with_pending_and_unapproved_calls
- ap08_restart_public_cancel_is_idempotent_without_active_runtime
- ap15_approved_before_claim_stopped_runtime_executes_once
- ap15_real_command_effect_then_runtime_stop_unknown
- ap16_finished_restart_never_reexecutes_and_missing_result_is_corrupt
- redacted_command_result_stays_finished_across_restart

接口依赖现集成 Store::execution_admission_status 与 Engine::new_recovered，无新增生产接口。静态 rustfmt 单文件及 git diff --check 通过；Cargo/真实进程均未执行，需经理验证原35例及实际holder收束。

后续日志静查确认 cli.rs 同域父Store未释放、network_authorization.rs 活controller recover/环境子进程父owner问题另批处理。host_configuration 两文件已转A，runtime/session归B，HTTP及lifecycle_compat归D，checkpoint/workspace三文件归A；C未修改这些文件。
