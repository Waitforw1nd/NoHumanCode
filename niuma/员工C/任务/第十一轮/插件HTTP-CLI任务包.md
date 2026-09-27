# C-R11-01 插件HTTP/CLI与联合验证任务

2026-09-27；唯一员工C/GPT-6-astra，执行标识C-R11-01-20260927-E01。本轮新任务，不恢复C-R10。

交付：契约三条HTTP路由及plugin list/status/set-enabled命令、读写token保护、严格DTO与固定安全错误、receipt/current分离、真实Host/CLI子进程证据。负责F05真实进程kill+wait后disabled保持、F09响应丢失手动原key回放及最终F11。非收敛200响应要完整输出JSON并CLI exit1，不将已接受命令误报未提交。

唯一写权：NoManCode/rust-app/src/server.rs、cli.rs、main.rs（必要时）；tests/plugin_http.rs、plugin_cli.rs（新）；tests/cli.rs、http_contract.rs仅必要fixture兼容且报告。其余产品只读，不改B/D模块/Cargo/build/web。准备依赖可独立的HTTP/CLI输入输出与测试，等B真实API由经理合入，禁止stub。若需cfg(test)私有Engine测试模块，交经理协调B而非越写lib/engine。

独立树../nhc-c-plugin-interface，分支codex/c/plugin-interface；提交执行人C，依赖与main整合人经理。报告niuma/员工C/提交报告/第十一轮/；review niuma/员工C/审查记录/第十一轮/。

固定组合未由经理明确SHA/tree并冻结前，仅本人定向测试，不先跑全五门禁。最终唯一由你串行标准check→test→fmt→clippy→wasm-check，失败立即停止后续，修改后完整从check重起。报告保留所有attempt的原始日志、真实UTC/命令/exit、未运行；发布副本相对路径，双hash。真实进程运行参数/数据目录/target/TMP/TEMP全E、临时端口、本机mock；禁止付费live/release。测试统计单列helper，不能拼组件结果。旧fmt空文件与435计数不当新事实。

## 所有员工的共同执行条件

主对话是经理，不接管产品实现。你是用户指定GPT-6-astra员工；必须核实际模型，不能静默换模。只使用经理指定的新E盘独立树，每次工具显式工作目录；D旧仓冻结，档案仅主仓niuma，不编辑本人树中档案副本。TMP/TEMP/target/原始日志配置到本人E树.local下，先准备工程rust-app/target目录以兼容旧fixture。现有Rust/MSVC只读，不安装更新工具；所有旧树/产物保留。不得reset/clean/stash/release/live/push。

启动读主仓AGENTS/HANDOFF/本人身份/开发Git规范/项目书现行目标/总进度及下方契约，已读不重复。按经理发送的完整SHA核HEAD、NoManCode/Rust tree、status/index。先回报真实接收、角色/模型/执行ID、路径、基线、所有权和阻塞，并更新主仓本人身份，再实施；旧代理ID不沿用成新任务接收证据。任务板记录若滞后本明确任务直接执行，不重复问用户。

完整基线与真实代理ID由主仓niuma/项目经理/任务/2026-09-27NEXT-10执行登记.md记录（管理包提交后补真实SHA，不自引用）；经理消息会明确指定它。不在基线未确定时开产品实现。员工仅暂存提交本人独立树明确源码，主仓任何index/commit及跨树merge都留经理。缺依赖继续本人可独立工作，禁止stub冒绿/跨写。公共接口争议先向经理给具体反例与建议，不自行改变冻结契约。

契约：[NEXT-10 v1.0](../../../项目经理/任务/2026-09-27NEXT-10插件持久管理契约.md)。报告落本人主仓新轮提交报告，review落本人主仓新轮审查记录；每实质节点更新本人身份，测试失败/未执行如实区分，完整SHA/tree/日志双hash绑定。历史NEXT-09已通过不重跑为接手证据，860根因未知。unknown不自动重试、先claim后副作用、批准与执行分离、恢复后原Task拒resume/旧grant不换绑保持。
