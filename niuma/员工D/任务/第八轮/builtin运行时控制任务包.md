# D-R8-01 固定builtin运行时控制任务

2026-09-27；唯一员工D/GPT-6-astra，执行标识D-R8-01-20260927-E01。本轮新任务，不恢复D-R7。

交付：tool_runtime的受限状态DTO、同一mutex管理控制闭包、持久状态驱动构造接口、真实fixed assembly启停/失败清理、文件lease RAII计数与F02/F04/F07/F10组件证据。先形成独立可编译组件小步SHA和精确API给经理/B，不能等待B先实现。D运行时DTO独立于B新模块，避免环依赖。

唯一写权：NoManCode/rust-app/src/tool_runtime.rs、tests/tool_runtime.rs及该模块内部测试。plugin_host.rs/plugin_catalog.rs只读；如现有Host语义无法满足，给经理具体反例与最小必要扩权，不自己修改。lib导出/Cargo/build不归D。

独立树../nhc-d-plugin-control，分支codex/d/plugin-control；提交执行人D，依赖与main整合人经理。报告niuma/员工D/提交报告/第八轮/；review niuma/员工D/审查记录/第八轮/。

公共Runtime API按契约with_files_control/FilesControl/builtin_with_enabled；状态字段请先精确通知B（结构/enum与serde稳定）。保持pub builtin及旧disable/unload组件接口兼容，raw PluginHost/Service不公开。snapshot映射错误仅安全cause_code，不传raw PluginError字符串。read/write停用绝不退compat；in_flight计数Drop不能重新取runtime锁。stop失败effect撤销但不得伪装正常stopped；Failed显式重建保留历史recovery。false初始装配不得先activate再stop。组件并发屏障必须有界超时/异常释放，不用无限等待。

可在本人组件内注入激活/停止失败和lease计数反例，不能新增生产env故障开关。你不执行最终组合五门禁；交组件候选后经理核review并合B/C，你继续限定只读复核或明确返工。

## 所有员工的共同执行条件

主对话是经理，不接管产品实现。你是用户指定GPT-6-astra员工；必须核实际模型，不能静默换模。只使用经理指定的新E盘独立树，每次工具显式工作目录；D旧仓冻结，档案仅主仓niuma，不编辑本人树中档案副本。TMP/TEMP/target/原始日志配置到本人E树.local下，先准备工程rust-app/target目录以兼容旧fixture。现有Rust/MSVC只读，不安装更新工具；所有旧树/产物保留。不得reset/clean/stash/release/live/push。

启动读主仓AGENTS/HANDOFF/本人身份/开发Git规范/项目书现行目标/总进度及下方契约，已读不重复。按经理发送的完整SHA核HEAD、NoManCode/Rust tree、status/index。先回报真实接收、角色/模型/执行ID、路径、基线、所有权和阻塞，并更新主仓本人身份，再实施；旧代理ID不沿用成新任务接收证据。任务板记录若滞后本明确任务直接执行，不重复问用户。

完整基线与真实代理ID由主仓niuma/项目经理/任务/2026-09-27NEXT-10执行登记.md记录（管理包提交后补真实SHA，不自引用）；经理消息会明确指定它。不在基线未确定时开产品实现。员工仅暂存提交本人独立树明确源码，主仓任何index/commit及跨树merge都留经理。缺依赖继续本人可独立工作，禁止stub冒绿/跨写。公共接口争议先向经理给具体反例与建议，不自行改变冻结契约。

契约：[NEXT-10 v1.0](../../../项目经理/任务/2026-09-27NEXT-10插件持久管理契约.md)。报告落本人主仓新轮提交报告，review落本人主仓新轮审查记录；每实质节点更新本人身份，测试失败/未执行如实区分，完整SHA/tree/日志双hash绑定。历史NEXT-09已通过不重跑为接手证据，860根因未知。unknown不自动重试、先claim后副作用、批准与执行分离、恢复后原Task拒resume/旧grant不换绑保持。
