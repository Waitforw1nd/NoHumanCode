# D-R9-01 文件发现Runtime服务

执行D-R9-01-20260927-E01，GPT-6-astra员工D，新实现任务；D-R9-PRE01只读建议已完成，旧D-R8不恢复。独立树../nhc-d-file-discovery，分支codex/d/file-discovery；提交人D，跨树整合人经理。完整基线待经理明确发送并核收。

唯一写权：src/tool_runtime.rs、tests/tool_runtime.rs及相关模块内测试，Host/catalog/Workspace/lib/Cargo只读。扩四真实服务与四项consumer绑定；manifest1.1.0、四interface key仍1.0.0；crate可见FILES_PLUGIN_VERSION、FILE_TOOLS、COMPAT_TOOLS按契约精确签名供B消费。现有control/DTO签名不变，不公开raw service。

优先交真实可编译API小步再补完整L01–L04组件：registry marker证明list/search来自注册，真实FS结果/无写scope只读正控、toolsfalse/越界/禁路径；disable/unload/failed零admit/无fallback，分别list/search旧lease禁用后可完与混合计数、错误/drop/abort归零；四effect循环/缺注册/失败rollback/recovery及N10-D01保持。同步FS无中途await，不承诺disable取消旧遍历，用有界测试服务停点与真实结果分别证明。

旧disabled list_files compat正控改成四项拒绝，另用有权限安全run_command组件正控；说明组件admit回调非真实审批，B负责Engine审批/ack。D-R9-PRE01报告保持原只读事实，不重写成实现交付。最终五门禁由C，不重复跑全量。
## 共同执行条件

先读主仓AGENTS/HANDOFF、本人身份、现行开发/Git规范和总进度，已读不重复。完整读取经理[NEXT-11契约](../../../项目经理/任务/2026-09-27NEXT-11文件发现服务化契约.md)。经理消息给完整开工SHA/tree后重新核本人独立树HEAD/status/index，先回复GPT-6-astra模型/新任务/执行ID/路径/范围真实接收，再实现；不是NEXT-10旧任务恢复。若实际模型不符立即报告，不能静默换模。

档案仅活动主仓niuma，报告/review按本新轮，不能编辑员工树档案副本。所有shell显式E工作目录；target/TMP/TEMP/log均本人E树.local，现有MSVC仅只读，不安装更新。独立树只提交本人明确源码；共享index/管理提交/跨树merge只经理。依赖缺失继续可独立工作，不stub、不复制/借用他人源码。

每固定候选交完整SHA、NoManCode/Rust tree、status/index、原始命令/真实UTC/exit、失败与未执行、原始/发布双hash与路径转换清单；源码/档案冻结交review。中间未提交工作区验证须如实标识，不能冒称提交后受测。不无故重跑继承门禁，正式最终只C固定组合串行check→test→fmt→clippy→wasm-check，失败立即停，改后从check重起。真crash是runtime停止或process kill+wait，不drop Arc。

保留先持久claim后副作用、批准/执行分离、unknown不重试、旧Task恢复封存/grant不换、Checkpoint Conflict优先/正常resume/complete合法新Turn。禁用不取消原合法lease；只读调用无审批claim。所有旧树/产物/日志/失败和c9154e5归档保留，无reset/clean/stash/release/live/push。超所有权改动先报具体原因等经理补充，不擅自扩范围。
