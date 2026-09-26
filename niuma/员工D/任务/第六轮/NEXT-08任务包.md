# D-R6-01 NEXT-08正式任务包

2026-09-27，经理GPT-6直接派发依据。当前状态：已正式派发并收到本人实际接收/干净基线回复，开始实现；后续状态见经理派发记录与本人身份。

执行标识 `D-R6-01-20260927-E01`，唯一员工 `/root/d_git_diff`，模型GPT-6-astra；独立树 `../nhc-d-workspace-identity`，分支 `codex/d/workspace-identity`。本人提交，经理review/串行整合。

完整开发基线 `80480c90fba51ec8aeadc01b2d8f184ae0bd1d5a`；NoManCode tree `0f01cd6c1621a16cef842a583028b91cc80f4e12`、Rust tree `f303e198698c8b68f11f937b6ec405fe8db6a70a`。这是NEXT-07已验收产品加NEXT-08冻结管理契约，建树前主线status/index空。契约：[NEXT-08 1.0](../../../项目经理/任务/2026-09-27NEXT-08多项目配置契约.md)，尤其§9精确API。

仅本任务独立树本人可按明确路径暂存/提交产品范围，整合人一律经理；禁止跨树merge、主树暂存/提交、reset/clean/stash、清理旧树、release/live/push。所有人员档案只写活动主仓 `niuma/员工D/`，不编辑独立树中档案副本，不提交共享人员档案。原执行者继续GPT-6-astra，不静默换模、不另开同任务执行者。

先返回真实接收、完整HEAD/tree/status、任务执行ID和独立树；再在共享本人身份追加接手卡。历史正文按字节保留，不通读重写换行。所有报告/命令/退出码/原始与发布日志双hash放本人新轮提交报告；出现失败保留原证据，不把未运行记通过。当前396/0/1是NEXT-07旧证据。

新Task/Turn权限上限、根身份与grant同事务，批准与执行分离、先claim后副作用、unknown不自动重试、恢复封存旧Task必须保持。schema11旧Task legacy_unbound只历史查询/纯回放/deny/cancel，旧恢复关闭是明确兼容变化；不可伪造历史root identity，也不可删掉新schema11正常恢复/crash正向测试。产品正式测试由员工执行，最终固定组合五门禁仅C一人串行。

关键节点提交固定源码候选，报告SHA/NoManCode与Rust tree/范围/未完成，再暂停本人源码供经理审查整合依赖；未验收不能自称APPROVED。若新增契约依赖及时报具体接口/文件，继续独立工作，不自行跨写。

## 本人范围与验收

实现纯workspace_identity模块及Windows真实目录身份测试。API按契约§9及本人只读预审：RootIdentity capture/verify/overlap、VerifiedRoot.path、RootOverlap枚举、verify_directory_policy；只读getter canonical_path方便B绑定。Windows卷/file ID+输入及canonical祖先reparse/case-sensitive核验，普通本机盘符，拒绝UNC/设备路径，无字符串身份降级。目录替换、根/祖先junction、大小写敏感、重复/嵌套/失效overlap必须有真实反例。UnixFixture真实dev/ino仅跨平台fixture，证据分开。

唯一产品文件（相对NoManCode/rust-app）：src/workspace_identity.rs、tests/workspace_identity.rs（均新增）、Cargo.toml、Cargo.lock（仅必要windows-sys FileSystem feature，无无关依赖升级）。不写lib/Workspace/Engine/Store/其他测试。独立测试可path导入本人模块，lib导出由B负责。

请优先交稳定模块与定向测试固定SHA供经理串行合给B/C；无法在当前权限建立的Windows特殊fixture如实记录，不把跳过记通过。重点P06，capture/verify前后祖先链观测、损坏JSON/版本/平台拒绝、非ASCII路径和CaseSensitive子目录策略。D交付后可只读审查B接入，但不跨写。

本人报告位置：活动共享仓库 `niuma/员工D/提交报告/第六轮/`；经理review：`niuma/员工D/审查记录/第六轮/`。
