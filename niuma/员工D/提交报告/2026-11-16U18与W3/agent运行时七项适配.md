# agent runtime 七项授权测试适配

依据冻结组合 discovery 的失败现场，只改 `src/agent_runtime_tests.rs`，保留原跨 Store 撤权、提交先后、子进程 kill、审批/消息/事件、raw 快照 hash 与新 turn 断言。适配七项：`n18_cross_store_disable_blocks_new_agent_lease_preserves_old_lease`、`n18_agent_admission_and_remote_disable_have_a_transaction_order`、`n19_cross_store_denial_without_notification_preserves_group_order`、`n19_admitted_read_finishes_after_real_engine_cancellation`、`n19_redacted_decision_process_kill_reconciles_once`、`n19_legacy_read_event_child_kill_then_reconcile_and_new_turn`、`n19_builtin_host_denied_after_commit_kill_recover_and_new_turn`。

已有 Engine 的同步和跨线程写入使用 A 的 `test_with_active_store_sync` 从同域现存 controller 取得真实 operation；read 完成测试持独立同域原票跨取消及结果持久化。子进程审批 deny/cancel 同样在其真实 controller 下取票。crash 后保持 raw 三件字节及 hash；仅 query/recovery 副本先核 version18 和 marker，明确构造 schema17 前体，Store 正式迁移建立该副本自身身份；不以复制的 schema18 身份冒认原域。这些副本验证迁移后的业务历史，不能当原 schema18 同域重开证据。

依赖 A 的测试 helper 和持久 schema18 迁移。仅执行 rustfmt 与 git diff --check；未运行 Cargo、子进程或真实测试，经理需在新固定候选窗口验证。

## 经理审查后更正

上文对 crash 后工作副本降为 schema17 的方案不被接受，已移除；不能以其结果代替 schema18 原域崩溃恢复。三个 crash 用例在 child kill/wait 后先逐文件原子保存未经改动的 raw/query/recovery 证据副本并登记哈希；后续查询、recover、Engine、重开均使用原 `agent.db` 路径和实际文件身份，Store 生产校验身份，断言 schema18 和 marker。raw 持续做原字节/hash 复核；原 operation scope 修正保留。无测试执行。

## 原 Child 收束修正

根据 C 只读审查，本文件 OwnedChild 在观测失败、超时或 kill 后迟到时报告 NHC_TERMINAL_UNKNOWN 并保留原对象；正常故意 kill 使用同一 Child 的有界 kill/try_wait，Drop 也不忽略不确定结果。三个 crash 的 barrier 截止改为保留原 Child，原数据留存逻辑不变；bounded helper 使用文件输出以免管道等待掩盖终止。此为静态候选，未执行真实进程。
