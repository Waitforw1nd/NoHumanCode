# B-R10-01 文件发现Host准入与持久兼容

执行B-R10-01-20260927-E01，GPT-6-astra员工B，新实现任务。独立树../nhc-b-file-discovery，分支codex/b/file-discovery；提交人B，依赖/main整合人经理。完整基线待经理管理归档后明确发送，未核收前不实现。

唯一写权：src/engine.rs、src/plugin_management.rs、tests/plugin_management.rs、tests/tool_session_turns.rs及相关模块内测试。Store/repository/schema/lib/approval/workspace和其余文件只读。D真实常量API合入后复用version/四FILE_TOOLS/二COMPAT_TOOLS，不自己造stub或重复硬编码当前清单。

交付Engine三读read/list/search最终同runtime锁内Store desired/权限重核；status当前1.1四服务信息，schema12原样兼容/旧receipt纯回放无协调、Task/grant/unknown保持。L02真实恶意list/search停用拒绝与无tool事件/续Provider，L03真实旧lease结果与runtime组件分工如实，L04命令按原授权/审批/网络ack正控（不要以run_wasm冒充可执行compat），L05全表保全与旧恢复回归。旧n10中disabled list_files成功断言现在须变为拒绝并保留独立合法command正控，不删除测试掩盖行为变化。

可先在本人树写三读准入和测试，D提交由经理普通merge后编译/组件验证；交可消费API版本，再完整矩阵。C最终组合验证，不由B先跑五门禁。
## 共同执行条件

先读主仓AGENTS/HANDOFF、本人身份、现行开发/Git规范和总进度，已读不重复。完整读取经理[NEXT-11契约](../../../项目经理/任务/2026-09-27NEXT-11文件发现服务化契约.md)。经理消息给完整开工SHA/tree后重新核本人独立树HEAD/status/index，先回复GPT-6-astra模型/新任务/执行ID/路径/范围真实接收，再实现；不是NEXT-10旧任务恢复。若实际模型不符立即报告，不能静默换模。

档案仅活动主仓niuma，报告/review按本新轮，不能编辑员工树档案副本。所有shell显式E工作目录；target/TMP/TEMP/log均本人E树.local，现有MSVC仅只读，不安装更新。独立树只提交本人明确源码；共享index/管理提交/跨树merge只经理。依赖缺失继续可独立工作，不stub、不复制/借用他人源码。

每固定候选交完整SHA、NoManCode/Rust tree、status/index、原始命令/真实UTC/exit、失败与未执行、原始/发布双hash与路径转换清单；源码/档案冻结交review。中间未提交工作区验证须如实标识，不能冒称提交后受测。不无故重跑继承门禁，正式最终只C固定组合串行check→test→fmt→clippy→wasm-check，失败立即停，改后从check重起。真crash是runtime停止或process kill+wait，不drop Arc。

保留先持久claim后副作用、批准/执行分离、unknown不重试、旧Task恢复封存/grant不换、Checkpoint Conflict优先/正常resume/complete合法新Turn。禁用不取消原合法lease；只读调用无审批claim。所有旧树/产物/日志/失败和c9154e5归档保留，无reset/clean/stash/release/live/push。超所有权改动先报具体原因等经理补充，不擅自扩范围。
