# agent runtime 七项授权测试适配

依据冻结组合 discovery 的失败现场，只改 `src/agent_runtime_tests.rs`，保留原跨 Store 撤权、提交先后、子进程 kill、审批/消息/事件、raw 快照 hash 与新 turn 断言。适配七项：`n18_cross_store_disable_blocks_new_agent_lease_preserves_old_lease`、`n18_agent_admission_and_remote_disable_have_a_transaction_order`、`n19_cross_store_denial_without_notification_preserves_group_order`、`n19_admitted_read_finishes_after_real_engine_cancellation`、`n19_redacted_decision_process_kill_reconciles_once`、`n19_legacy_read_event_child_kill_then_reconcile_and_new_turn`、`n19_builtin_host_denied_after_commit_kill_recover_and_new_turn`。

已有 Engine 的同步和跨线程写入使用 A 的 `test_with_active_store_sync` 从同域现存 controller 取得真实 operation；read 完成测试持独立同域原票跨取消及结果持久化。子进程审批 deny/cancel 同样在其真实 controller 下取票。crash 后保持 raw 三件字节及 hash；仅 query/recovery 副本先核 version18 和 marker，明确构造 schema17 前体，Store 正式迁移建立该副本自身身份；不以复制的 schema18 身份冒认原域。这些副本验证迁移后的业务历史，不能当原 schema18 同域重开证据。

依赖 A 的测试 helper 和持久 schema18 迁移。仅执行 rustfmt 与 git diff --check；未运行 Cargo、子进程或真实测试，经理需在新固定候选窗口验证。
