# C-R12-01 文件发现HTTP/CLI与最终验证

执行C-R12-01-20260927-E01，GPT-6-astra员工C，新实现任务。独立树../nhc-c-file-discovery，分支codex/c/file-discovery；提交人C，依赖/main整合人经理。完整基线待经理明确发送并核收。

唯一写权：src/cli.rs、tests/plugin_http.rs、tests/plugin_cli.rs。server/main/其余代码只读，现有HTTP DTO路由不需扩改。CLI精确支持1.0旧清单与1.1新清单，拒未知或跨版本混搭，human按validated当前DTO显示影响/compat；JSON和unknown/no retry/unconverged exit1保持。不要放开任意version/工具数组。

负责L06/L07：真实HTTP1.1四affected/二compat/four providers；CLI真实进程验证旧1.0和新1.1精确映射、人类输出、混搭拒绝及原安全契约。新list/search真实Host kill+wait/restart disabled保持，零启动Provider；禁用时恶意调用严格未授权failed/无工具结果，显式enable后的独立新Run真实list/search正控。保留原read/write与失响应回放测试，不将reopen或drop Arc称crash。

先独立写CLI与测试，经理合D→B真实依赖后专项验证；源码冻结交具体SHA/证据。最终只有收到经理固定组合完整SHA/tree的明确冻结消息才唯一串行标准五门禁；失败停后续，改后check重起。元数据含前后完整树、UTC、exit、真实原始零字节fmt文件，原始/发布hash与统计helper分列，不重复累加child输出。旧fixture的rust-app/target准备，禁止release/live。
## 共同执行条件

先读主仓AGENTS/HANDOFF、本人身份、现行开发/Git规范和总进度，已读不重复。完整读取经理[NEXT-11契约](../../../项目经理/任务/2026-09-27NEXT-11文件发现服务化契约.md)。经理消息给完整开工SHA/tree后重新核本人独立树HEAD/status/index，先回复GPT-6-astra模型/新任务/执行ID/路径/范围真实接收，再实现；不是NEXT-10旧任务恢复。若实际模型不符立即报告，不能静默换模。

档案仅活动主仓niuma，报告/review按本新轮，不能编辑员工树档案副本。所有shell显式E工作目录；target/TMP/TEMP/log均本人E树.local，现有MSVC仅只读，不安装更新。独立树只提交本人明确源码；共享index/管理提交/跨树merge只经理。依赖缺失继续可独立工作，不stub、不复制/借用他人源码。

每固定候选交完整SHA、NoManCode/Rust tree、status/index、原始命令/真实UTC/exit、失败与未执行、原始/发布双hash与路径转换清单；源码/档案冻结交review。中间未提交工作区验证须如实标识，不能冒称提交后受测。不无故重跑继承门禁，正式最终只C固定组合串行check→test→fmt→clippy→wasm-check，失败立即停，改后从check重起。真crash是runtime停止或process kill+wait，不drop Arc。

保留先持久claim后副作用、批准/执行分离、unknown不重试、旧Task恢复封存/grant不换、Checkpoint Conflict优先/正常resume/complete合法新Turn。禁用不取消原合法lease；只读调用无审批claim。所有旧树/产物/日志/失败和c9154e5归档保留，无reset/clean/stash/release/live/push。超所有权改动先报具体原因等经理补充，不擅自扩范围。
