# 员工 D · 第二轮任务提示词

日期：2026-09-23；D-R2-01 / NEXT-02B，修订1。经理已准备，待用户分发；尚无实际执行者。执行模型由用户分发时确定（上轮为 SWE2max，本任务建议延续同线）；这不是经理已调用模型或员工已开工的记录。

```text
你担任NoHumanCode员工D，执行D-R2-01 / NEXT-02B修订1：插件Host注册表与可撤销生命周期（builtin）。本任务消费你在 NEXT-02A 已验收入库的 plugin_catalog（基线 b67fedf）。执行模型以用户分发时指定为准；无论哪个模型，开发与验收规则不变。

仓库根为 ./，含AGENTS.md、PROJECT-BOOK.md、NoManCode/、niuma/。源码NoManCode，Rust工程NoManCode/rust-app，个人资料niuma/员工D。先按根AGENTS与员工启动提示词读取本人身份、项目开发守则、Git协作规范、路径与可移植性、项目书现行说明和§3.1～3.4/§5/§10、经理最新交接；然后完整读取：
niuma/员工D/任务/第二轮/插件Host注册表与生命周期契约.md
已读不重复，不从完整聊天开始。

用户将本有效提示词交给你即是派发依据。登记任务修订、来源、接手时间和唯一执行标识到本人身份，再直接实施，不因任务板滞后重新索取确认。同任务已有执行者时核对接续，不另开重复实现。

本任务是首个独立工作树任务。基线 b67fedfcd6ca35969363096ea64ddd5fb1290524；分支 codex/d/plugin-host；工作树在仓库旁 ../nhc-d-plugin-host/（HEAD 已是基线）。你在该工作树的分支上自行提交本人范围内的小步修改；共享树 ./ 的暂存/提交/整合仍只有经理执行。报告与身份写共享树 ./niuma/员工D/…，不编辑工作树内的 niuma 副本。

你唯一可写的源码为工作树 NoManCode/rust-app 内：新 src/plugin_host.rs、新 tests/plugin_host.rs，以及 src/lib.rs 仅追加 pub mod plugin_host; 一行。plugin_catalog.rs 只读依赖（API 缺口写进报告由经理修订）；wasm.rs、server/turn_http、engine/store/repository/domain/secrets/provider/workspace、crates、Cargo/lock、脚本、旧测试、web、他人档案均不写。仅用 std+serde/serde_json，不新增依赖。新模块写完整再接导出。

实现契约规定的 PluginHost（一 scope 一目录）、start 当场 resolve 并按计划序激活、BuiltinPlugin trait 与注入工厂（builtin 无工厂 MissingFactory，wasm/process 为 UnsupportedRuntime）、受限 PluginContext（bound 仅限已声明且计划绑定的 requires；register_effect 仅限已声明 provides，activate 结束校验 provides 全覆盖）、六种 effect 注册与事件订阅/emit（按订阅者 id 序投递）、Registered→Loaded→Active→Stopped→Unloaded/Failed 状态机、stop 的 DependentsActive 保护与 stop_subtree 逆序级联、unload 仅限非 Active、deactivate/panic 经 catch_unwind 隔离并 Failed+pending_recovery 显式登记（注册表始终无残留）、HostError typed 变体。激活失败按计划逆序回滚并列 unwound。display_name 不参与身份；host 本体不读写文件/网络/数据库/进程/环境。

按契约 H01～H12 用公开 API 完成集成测试与必要单测：真实计数器服务调用、拓扑/逆序、撤销无残留、依赖保护、激活失败回滚、声明违背、不支持运行时、状态与作用域隔离、事件投递与停投、恢复清单、纯度与 display_name 非身份、既有门禁回归。

验证在工作树 ../nhc-d-plugin-host/NoManCode/rust-app 直接执行（独立 target，无需冻结副本）；本机 pwsh 是 Git 内嵌 shim（PS 5.1）、VS 非标准位置，按联合门禁记录注入 VCToolsInstallDir/VSINSTALLDIR/WindowsSdkDir 并把 Git 目录前置 PATH。同一 pwsh 进程先 build.ps1 -Action check 建环境，再跑契约所列定向测试与 clippy；交付前对工作树分支候选完整跑 test/fmt/clippy/wasm-check（fmt 仅检查），不跑 build（复制 EXE）、不跑付费 live。git diff b67fedf 只含授权三处。

交付共享树 niuma/员工D/提交报告/第二轮/员工D（插件Host注册表与生命周期 第1.0轮报告）.md，附件 D-R2-01证据/（脱敏、相对路径）。逐项给 H01～H12 入口/测试/结果、分支候选提交完整 SHA、实际模型、修改清单、失败/未运行项与后续范围；更新本人身份并将报告路径交回经理。你在工作树分支的提交不等于经理验收或整合。
```

完整 [契约与验收](插件Host注册表与生命周期契约.md)。
