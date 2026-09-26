# B-R7-01 NEXT-08正式任务包

2026-09-27，经理GPT-6直接派发依据。当前状态：已正式派发并收到本人实际接收/干净基线回复，开始实现；后续状态见经理派发记录与本人身份。

执行标识 `B-R7-01-20260927-E01`，唯一员工 `/root/b_checkpoint`，模型GPT-6-astra；独立树 `../nhc-b-project-context`，分支 `codex/b/project-context`。本人提交，经理review/串行整合。

完整开发基线 `80480c90fba51ec8aeadc01b2d8f184ae0bd1d5a`；NoManCode tree `0f01cd6c1621a16cef842a583028b91cc80f4e12`、Rust tree `f303e198698c8b68f11f937b6ec405fe8db6a70a`。这是NEXT-07已验收产品加NEXT-08冻结管理契约，建树前主线status/index空。契约：[NEXT-08 1.0](../../../项目经理/任务/2026-09-27NEXT-08多项目配置契约.md)，尤其§9精确API。

仅本任务独立树本人可按明确路径暂存/提交产品范围，整合人一律经理；禁止跨树merge、主树暂存/提交、reset/clean/stash、清理旧树、release/live/push。所有人员档案只写活动主仓 `niuma/员工B/`，不编辑独立树中档案副本，不提交共享人员档案。原执行者继续GPT-6-astra，不静默换模、不另开同任务执行者。

先返回真实接收、完整HEAD/tree/status、任务执行ID和独立树；再在共享本人身份追加接手卡。历史正文按字节保留，不通读重写换行。所有报告/命令/退出码/原始与发布日志双hash放本人新轮提交报告；出现失败保留原证据，不把未运行记通过。当前396/0/1是NEXT-07旧证据。

新Task/Turn权限上限、根身份与grant同事务，批准与执行分离、先claim后副作用、unknown不自动重试、恢复封存旧Task必须保持。schema11旧Task legacy_unbound只历史查询/纯回放/deny/cancel，旧恢复关闭是明确兼容变化；不可伪造历史root identity，也不可删掉新schema11正常恢复/crash正向测试。产品正式测试由员工执行，最终固定组合五门禁仅C一人串行。

关键节点提交固定源码候选，报告SHA/NoManCode与Rust tree/范围/未完成，再暂停本人源码供经理审查整合依赖；未验收不能自称APPROVED。若新增契约依赖及时报具体接口/文件，继续独立工作，不自行跨写。

## 本人范围与验收

实现ProjectConfig/Root sidecar/schema11迁移、Engine新项目入口、旧start当前选择解析、Session原项目续Turn、Workspace/Diff/Checkpoint/审批/恢复根验证和当前权限上限、项目网络授权命名空间与Task固定grant。新增project_config模块；保持旧JSON/digest/密文。所有同步/异步签名按契约§9，新增NetworkError项目变体保留calls/dispatch。

唯一产品文件（相对NoManCode/rust-app）：src/project_config.rs、src/domain.rs、src/repository.rs、src/store.rs、src/engine.rs、src/network_authorization.rs、src/lib.rs、src/workspace.rs、src/tool_runtime.rs；tests/project_context.rs（新增）、tests/project_migration.rs（新增）、tests/support/network_fixture.rs、tests/support/project_fixture.rs（可新增）、tests/network_authorization.rs、tests/provider_network.rs、tests/checkpoint.rs、tests/workspace_changes.rs、tests/workspace_crash.rs、tests/approval_gate.rs、tests/session_turns.rs、tests/tool_session_turns.rs、tests/tool_runtime.rs、tests/runtime.rs、tests/assessment_adversarial.rs、tests/final_acceptance.rs、tests/live.rs（仅fixture/编译适配，忽略保持）、tests/git_diff.rs、tests/plugin_host.rs、tests/plugin_catalog.rs。后三项仅必要fixture适配，不扩功能。不得写server/cli/web/C测试、D模块或Cargo。

D模块由经理合入依赖，lib导出归你；C接口也由经理串行合，不能临时在你树写C stub。先做好类型/事务主体并给最早可消费固定SHA；完整编译若因尚未合C对NetworkSubject适配受阻，记录缺依赖，不降级类型契约。公共fixture必须走真实生产register/configure/网络确认，不能给旧Task任意补sidecar的生产后门。重点P01–P08/P10，真实迁移所有旧列一致、撤权/claim竞态和恢复在A/B隔离。

本人报告位置：活动共享仓库 `niuma/员工B/提交报告/第七轮/`；经理review：`niuma/员工B/审查记录/第七轮/`。
