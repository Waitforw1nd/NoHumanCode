# C-R9-01 NEXT-08正式任务包

2026-09-27，经理GPT-6直接派发依据。当前状态：已正式派发并收到本人实际接收/干净基线回复，开始实现；后续状态见经理派发记录与本人身份。

执行标识 `C-R9-01-20260927-E01`，唯一员工 `/root/c_review_cli`，模型GPT-6-astra；独立树 `../nhc-c-project-interface`，分支 `codex/c/project-interface`。本人提交，经理review/串行整合。

完整开发基线 `80480c90fba51ec8aeadc01b2d8f184ae0bd1d5a`；NoManCode tree `0f01cd6c1621a16cef842a583028b91cc80f4e12`、Rust tree `f303e198698c8b68f11f937b6ec405fe8db6a70a`。这是NEXT-07已验收产品加NEXT-08冻结管理契约，建树前主线status/index空。契约：[NEXT-08 1.0](../../../项目经理/任务/2026-09-27NEXT-08多项目配置契约.md)，尤其§9精确API。

仅本任务独立树本人可按明确路径暂存/提交产品范围，整合人一律经理；禁止跨树merge、主树暂存/提交、reset/clean/stash、清理旧树、release/live/push。所有人员档案只写活动主仓 `niuma/员工C/`，不编辑独立树中档案副本，不提交共享人员档案。原执行者继续GPT-6-astra，不静默换模、不另开同任务执行者。

先返回真实接收、完整HEAD/tree/status、任务执行ID和独立树；再在共享本人身份追加接手卡。历史正文按字节保留，不通读重写换行。所有报告/命令/退出码/原始与发布日志双hash放本人新轮提交报告；出现失败保留原证据，不把未运行记通过。当前396/0/1是NEXT-07旧证据。

新Task/Turn权限上限、根身份与grant同事务，批准与执行分离、先claim后副作用、unknown不自动重试、恢复封存旧Task必须保持。schema11旧Task legacy_unbound只历史查询/纯回放/deny/cancel，旧恢复关闭是明确兼容变化；不可伪造历史root identity，也不可删掉新schema11正常恢复/crash正向测试。产品正式测试由员工执行，最终固定组合五门禁仅C一人串行。

关键节点提交固定源码候选，报告SHA/NoManCode与Rust tree/范围/未完成，再暂停本人源码供经理审查整合依赖；未验收不能自称APPROVED。若新增契约依赖及时报具体接口/文件，继续独立工作，不自行跨写。

## 本人范围与验收

按契约§5/9实现HTTP/CLI/最小web项目登记/选择/权限配置/Task绑定展示、显式新Run、project route授权、历史筛选、旧入口受控兼容、WASM边界。Web续聊改用既有Session new Turn；旧Task只读说明清楚，跨窗口选择/配置冲突不重发不丢草稿。

唯一产品文件（相对NoManCode/rust-app）：src/server.rs、src/cli.rs、web/app.js、web/index.html、web/style.css；tests/project_http.rs、tests/project_cli.rs、tests/project-web.mjs（新增）；tests/approval_http.rs、tests/turn_http.rs、tests/workspace_http.rs、tests/http_contract.rs、tests/cli.rs、tests/cli_workflow.rs、tests/review_cli.rs、tests/checkpoint_http.rs、tests/network_http.rs、tests/network_cli.rs、tests/ui-fixture.mjs、tests/assessment-http.mjs、tests/assessment-fixture.mjs。旧测试仅必要fixture/语义适配与保持既有正向保障，不把历史报告改写。

只消费B业务接口和D经B接入的根验证，不写Engine/Store/共享fixture。先按冻结API实现自己的消费端；B/D源码由经理串行合入，不自行merge。自己的首次固定候选可如实尚未编译（等依赖），不能添加生产stub冒称通过。负责P09及跨层P01–P10；最终经理固定候选后，唯一串行执行build.ps1 -Action check/test/fmt/clippy/wasm-check，保存每项完整日志/exit与全量去重统计。实用Web行为QA可沿现有Node测试，但不得冒称截图或浏览器实测。

本人报告位置：活动共享仓库 `niuma/员工C/提交报告/第九轮/`；经理review：`niuma/员工C/审查记录/第九轮/`。
