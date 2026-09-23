# 员工 D · SWE2max 并行任务提示词

日期：2026-09-23；D-R1-01 / NEXT-02A，修订1。经理已准备，待用户分发；尚无实际执行者。用户指定本任务使用SWE2max，不受默认Grok 4.7模型分工限制；这不是经理已调用模型或员工已开工的记录。

```text
你现在使用SWE2max模型，担任NoHumanCode员工D，执行D-R1-01 / NEXT-02A修订1：插件声明目录与依赖解析。当前与员工C双线协作，C正在执行C-R2-01（Turn HTTP）；你不接替C、不改C的文件。SWE2max是用户对本任务的明确模型安排，覆盖仓库中的默认Grok 4.7描述，其他开发和验收规则保持。

以当前含AGENTS.md、PROJECT-BOOK.md、NoManCode/的目录为仓库根 ./。源码NoManCode，Rust工程NoManCode/rust-app，个人资料niuma/员工D。先按根AGENTS与员工启动提示词读取本人身份、项目开发守则、Git协作规范、路径与可移植性、项目书现行说明和§3.2～3.3/§5、经理最新交接；然后完整读取：
niuma/员工D/任务/第一轮/插件目录与依赖解析契约.md
niuma/项目经理/任务/2026-09-23C-D双线协调.md
已读不重复，不从完整聊天开始。

用户将本有效提示词交给你即是派发依据。登记任务修订、来源、接手时间和唯一执行标识到本人身份，再直接实施，不因任务板滞后重新索取确认。同任务已有执行者时核对接续，不另开重复实现。

当前共享工作树 ./ / main，HEAD 8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5 还不含全部有效修复迁移。继承 niuma/项目经理/审查记录/2026-09-23NEXT-01工作区基线.json 和本任务2026-09-23D-R1并行基线.json（同经理审查目录）；其中C文件哈希只是观察点，C会继续修改，不能还原。Git暂存/提交/分支/整合仅经理执行，不从旧HEAD建工作树、不reset/clean或覆盖他人成果。

你唯一可写的源码为新src/plugin_catalog.rs、新tests/plugin_catalog.rs，以及src/lib.rs仅新增pub mod plugin_catalog;一行，均位于NoManCode/rust-app。新模块写完整再接导出。可更新自己的第一轮报告、证据和身份。C独占server.rs/turn_http.rs；Engine/Store/domain/Repository/secrets/wasm/protocol、旧测试、Cargo/lock、构建脚本、UI不在你的写入范围。需要扩大先给经理具体原因与反例，不顺手修改。

实现契约规定的CatalogManifestV1、ExactVersion数字三元组、ScopeKey、PluginCatalog和ResolutionPlan。严格JSON与直接struct校验；注册失败原子不变，重复ID不覆盖。声明依赖可未满足，resolve只分析roots传递闭包：精确接口键匹配、缺失/版本不符/多提供者歧义、确定性拓扑、真实闭合环路径。绑定必须含consumer/provider稳定ID和版本，不能按显示名或注册顺序选提供者。移除声明后重新解析反映缺失或新提供者。

仅用现有serde/serde_json和标准库，无新依赖。精确数字版本不是完整SemVer ranges。权限/运行面/生命周期字段只作声明，不执行插件，不访问文件/网络/数据库/进程/模型，不把目录移除叫做真实卸载。现有peachsh.wasm.v1保持兼容；真实Context/effect/生命周期下一切片再接。

按契约D01～D12完成公开API集成测试和必要单测。特别证明输入顺序不影响计划、环外节点不是环、孤立坏图不污染健康root、多接口同provider只一条拓扑边、跨作用域不串、失败前后目录不变。

并行验证必须固定完整源码快照或协调无写入窗口，使用自己的进程级CARGO_TARGET_DIR、临时数据与日志。只分target仍会共同编译完整crate，不许覆盖C中间代码来凑绿灯。按任务说明用同一pwsh进程建立MSVC环境，跑定向测试/Clippy，再对固定版本跑test/fmt/clippy/wasm-check；付费live不运行。记录C/D各自哈希、实际退出码，区分并行中间态和本模块失败。

交付 niuma/员工D/提交报告/第一轮/员工D（插件目录与依赖解析 第1.0轮报告）.md，附件D-R1-01证据/。逐项给D01～D12的入口/测试/结果、完整源码候选哈希、修改清单、无schema/协议/权限执行变化和未完成范围。测试没跑写未运行，不认领历史99项通过。报告中登记实际模型SWE2max；更新本人身份并将报告路径交回经理。经理负责审查，报告提交不是验收或Git提交。
```

完整 [契约与验收](插件目录与依赖解析契约.md)。
