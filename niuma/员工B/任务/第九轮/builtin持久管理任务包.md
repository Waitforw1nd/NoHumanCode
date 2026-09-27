# B-R9-01 持久插件管理任务与执行提示词

2026-09-27；唯一员工B/GPT-6-astra，执行标识B-R9-01-20260927-E01。本轮新任务，不恢复B-R8。

交付：schema12固定builtin.files期望状态/CAS/幂等事实、Engine诊断与管理命令、最终claim复核、启动按持久状态装配及F01/F03/F04/F06/F07/F08证据。先做独立DTO/Store/迁移；D的真实组件合入后完成Engine。负责F04真实write与持久receipt，不能仅依赖D内存marker测试。针对F07 commit后apply前设计cfg(test)有界停点，真实runtime shutdown或child kill+wait，不drop Arc冒充。

唯一写权：NoManCode/rust-app/src/plugin_management.rs（新）、engine.rs、store.rs、repository.rs、lib.rs、tests/plugin_management.rs（新），含本人模块内相关测试。不能改D tool_runtime或C server/cli，不能改Cargo、build、其他旧测试。src/lib.rs新模块导出仅你维护；必要cfg(test)测试组织也归你。

独立树../nhc-b-plugin-management，分支codex/b/plugin-management；提交执行人B，依赖与main整合人经理。报告niuma/员工B/提交报告/第九轮/；review niuma/员工B/审查记录/第九轮/。

先交公共DTO/签名与持久层可审小步SHA，通知C消费；经理合D真实依赖后再交组合候选。组件定向测试按实际必要性运行，不抢C最终五门禁。注意迁移11 verify上界、严格约束、同key回放先于CAS、same enabled新key revision+1、DB失败runtime不动、管理runtime→Store锁序、claim事务内enabled检查。命令receipt只证明期望持久提交，不证明runtime active；纯回放不得再次协调。

## 所有员工的共同执行条件

主对话是经理，不接管产品实现。你是用户指定GPT-6-astra员工；必须核实际模型，不能静默换模。只使用经理指定的新E盘独立树，每次工具显式工作目录；D旧仓冻结，档案仅主仓niuma，不编辑本人树中档案副本。TMP/TEMP/target/原始日志配置到本人E树.local下，先准备工程rust-app/target目录以兼容旧fixture。现有Rust/MSVC只读，不安装更新工具；所有旧树/产物保留。不得reset/clean/stash/release/live/push。

启动读主仓AGENTS/HANDOFF/本人身份/开发Git规范/项目书现行目标/总进度及下方契约，已读不重复。按经理发送的完整SHA核HEAD、NoManCode/Rust tree、status/index。先回报真实接收、角色/模型/执行ID、路径、基线、所有权和阻塞，并更新主仓本人身份，再实施；旧代理ID不沿用成新任务接收证据。任务板记录若滞后本明确任务直接执行，不重复问用户。

完整基线与真实代理ID由主仓niuma/项目经理/任务/2026-09-27NEXT-10执行登记.md记录（管理包提交后补真实SHA，不自引用）；经理消息会明确指定它。不在基线未确定时开产品实现。员工仅暂存提交本人独立树明确源码，主仓任何index/commit及跨树merge都留经理。缺依赖继续本人可独立工作，禁止stub冒绿/跨写。公共接口争议先向经理给具体反例与建议，不自行改变冻结契约。

契约：[NEXT-10 v1.0](../../../项目经理/任务/2026-09-27NEXT-10插件持久管理契约.md)。报告落本人主仓新轮提交报告，review落本人主仓新轮审查记录；每实质节点更新本人身份，测试失败/未执行如实区分，完整SHA/tree/日志双hash绑定。历史NEXT-09已通过不重跑为接手证据，860根因未知。unknown不自动重试、先claim后副作用、批准与执行分离、恢复后原Task拒resume/旧grant不换绑保持。
