# 员工D（插件Host注册表与生命周期 第1.0轮报告）

日期：2026-09-24
任务：D-R2-01 / NEXT-02B 修订1（插件Host注册表与可撤销生命周期·builtin）
执行标识：D-R2-01-20260924-0355
实际模型：SWE2max（用户对本任务的明确指定）
执行范围：仅工作树 `../nhc-d-plugin-host/`（分支 `codex/d/plugin-host`）内 `NoManCode/rust-app/src/plugin_host.rs`（新）、`NoManCode/rust-app/tests/plugin_host.rs`（新）、`NoManCode/rust-app/src/lib.rs` 追加 `pub mod plugin_host;` 一行。plugin_catalog.rs 只读未改；wasm.rs、server.rs/turn_http.rs、engine/store/repository/domain/secrets/provider/workspace、crates、Cargo.toml/Cargo.lock、脚本、既有测试、web 及他人 niuma 档案均未触碰；工作树内 niuma 副本未编辑。

## 交付物与候选提交

- 基线：`b67fedfcd6ca35969363096ea64ddd5fb1290524`
- 分支：`codex/d/plugin-host`
- 候选提交：`1223f6b80591eb5c54e480f09991f67e525e3b08`（feat(plugin): add builtin plugin host registry and revocable lifecycle；2026-09-24 20:14 +0800）
- `git diff b67fedf..HEAD --name-only` 仅含下列三处（核对通过）

| 文件 | 状态 | sha256 |
| --- | --- | --- |
| `NoManCode/rust-app/src/plugin_host.rs` | 新增（1488 行） | `f135627041ccc5d0c245be7c315b5020817569e8be495a268d9fcf8daaf16094` |
| `NoManCode/rust-app/tests/plugin_host.rs` | 新增（2323 行） | `e0b8755daa541114aa0e350688194d60eef8b26fab87309827d1df00656211f0` |
| `NoManCode/rust-app/src/lib.rs` | 仅追加 `pub mod plugin_host;` 一行 | `c8c528a4fc0d2fa41156d275a2bb3b7a63982096aef72cc48d7b8c493f33c87c` |

## 实现说明

- 公开 API：`PluginHost`（`new`/`catalog`/`catalog_mut`/`with_factory`/`resolve`/`start`/`stop`/`stop_subtree`/`unload`/`emit`/`instances`/`effects`/`pending_recovery`）、`BuiltinPlugin` trait、`PluginContext`（`plugin_id`/`scope`/`manifest`/`bound`/`register_effect`/`unregister_effect`/`subscribe`/`emit`）、`BoundInterface`、`EffectId`、`InstanceState`（Registered/Loaded/Active/Stopped/Unloaded/Failed{stage}）、`FailureStage`（Activate/Deactivate/Deliver）、`StartReport{activated,reused}`、`InstanceSnapshot`、`EffectRecord`、`RecoveryItem`、`PluginError`、`PluginFailureCause`、`HostError` typed 变体全族。
- 生命周期：`start` 当场 `catalog.resolve` 并按 `ordered_plugins` 顺序激活；无实例/Stopped→工厂取新对象 Loaded→Active；Active 非 root→`reused` 不重复激活；Active/Failed root→`InvalidState`；Failed 非 root 成员→`InvalidState` 并回滚本批。每次 Loaded→Active 都经工厂取新对象，Stopped 后旧对象已释放不复用。
- 受限上下文：`bound` 仅限 manifest.requires 内且激活计划绑定的 key（返回 provider 已注册 effect 的 `&dyn Any`，含稳定 provider_id）；`register_effect` 仅限 manifest.provides 内，重复注册 `DuplicateEffect`；`unregister_effect` 仅限自身 effect；`subscribe` 仅限 requires 内已绑定且 kind==event 的 key；`ctx.emit`/`host.emit` 仅限自身 provides 内 kind==event 的 key，按订阅者插件 id 升序同步投递，emit 之后的订阅不补投。
- 订阅与 provider 绑定：订阅 effect 记录激活计划中的 provider_id；emit 时仅向 provider_id==emitter 的订阅投递——同 key 第二提供者后续激活并 emit 不串台（`subscribers_receive_only_their_bound_providers_events` 证明）。
- 状态机与撤销：`stop` 有 Active 消费者→`DependentsActive`（id 升序）；否则 catch_unwind 下 deactivate→强制回收全部 effect→Stopped。`stop_subtree` 按激活逆序级联。`unload` 拒绝 Active，Failed 的残留转恢复清单登记。激活失败按计划逆序回滚本批实例并列 `unwound`。deactivate Err/panic→Failed{Deactivate}+恢复清单，注册表侧始终无残留。
- panic 隔离：activate/deactivate 与订阅回调均经 `catch_unwind`；订阅回调 panic 归因于订阅者自身——Failed{Deliver}+恢复清单登记+撤销其 effects，不抛回公共 API、不归咎 emitter，健康订阅者继续收到后续投递；emit 返回值为成功投递的回调数。
- 确定性/纯度：BTreeMap/排序遍历，同输入同激活与撤销顺序；host 本体无文件/网络/数据库/进程/时钟/环境变量读取；display_name 不参与身份、排序或依赖；permissions/config_schema 仅记录不执行；`peachsh.wasm.v1` 未接入不受影响。

## H01–H12 验收逐项

全部位于 `tests/plugin_host.rs`（公开 API 集成测试，23 个）；模块内另有 6 个 `#[cfg(test)]` 单测辅助覆盖内部规则。

| 条目 | 测试入口 | 关键断言 | 结果 |
| --- | --- | --- | --- |
| H01 完整链 | `h01_full_chain_register_start_bound_call` | 激活顺序 provider→consumer；consumer 经 `ctx.bound` downcast 取得真实 `Arc<CounterService>` 并成功调用（hits=1）；`bound.provider_id()=="svc.provider"`；instances/effects 快照完整且确定序 | 通过 |
| H02 拓扑序 | `h02_topology_order_and_reverse_teardown` | 链+菱形+多 roots：激活序==计划序（消费者必在其 provider 之后）；`stop_subtree` 逆序消费者先于提供者；roots 顺序/注册顺序扰动不改变序 | 通过 |
| H03 撤销 | `h03_revocation_leaves_no_residue` | `stop_subtree(provider)` 后 provider 全部 effect 消失、consumer 已停且其订阅移除；instances/effects 无残留；unload 后实例记录消失 | 通过 |
| H04 依赖保护 | `h04_dependents_active_protects_provider` | 直接 `stop(provider)`→`DependentsActive{dependents}` 按 id 排序；双方状态不变 | 通过 |
| H05 激活失败 | `h05_activation_failure_rolls_back_in_plan_reverse` | activate Err→`PluginStartError{plugin_id,stage:Activate,cause,unwound}`；panic→`PluginPanic{stage:Activate}`；已激活 provider 逆序回滚、effects 无残留、unwound 准确 | 通过 |
| H06 声明违背 | `h06_declaration_violations_are_typed_and_rolled_back` | 注册未声明 provides→`UndeclaredInterface`；漏注册已声明 provides→`ProvidesNotCovered{missing}`；整体回滚无残留 | 通过 |
| H07 运行时 | `h07_runtime_gates` | wasm/process 声明→`UnsupportedRuntime`；builtin 无工厂→`MissingFactory`；均不实例化、无 effect、无调用、带 unwound | 通过 |
| H08 状态与作用域 | `h08_state_rules_and_scope_isolation` | 重复 start 同 root→`InvalidState`；stop 已停→`InvalidState`；unload Active→`InvalidState`；未知 id→`UnknownPlugin`；Project/Session 两个同名 scope host 互不满足、互不影响 | 通过 |
| H09 事件 | `h09_events_deliver_in_subscriber_id_order` | 按订阅者 id 升序投递（与注册顺序无关）、计数正确；emit 后创建的订阅不补投；consumer 停后其订阅移除且不再收到；provider 停后 emit→`InvalidState` 零投递 | 通过 |
| H10 恢复清单 | `h10_recovery_list_records_incomplete_cleanup` | deactivate Err→`PluginDeactivateError`、panic→`PluginPanic{stage:Deactivate}`；实例 Failed{Deactivate}、注册表无残留、恢复清单按序登记；unload(Failed) 后仍登记且实例移除 | 通过 |
| H11 纯度/身份 | `h11_purity_determinism_and_display_name_not_identity` | `resolve` 重复调用计划全量相等；未声明/无绑定 key→`UnboundInterface`；同 display_name 不同 id 各自独立 | 通过 |
| H12 回归 | `h12_existing_surfaces_unaffected` + 门禁表 + `git diff` 核对 | 全 workspace test/fmt/clippy/wasm-check 全绿；`git diff b67fedf..HEAD` 仅授权三处；catalog/wasm/其他源码零改动 | 通过 |

## 契约规则反例补充（契约 §3 各规则的直接反例）

- `ctx_rule_boundaries_reject_with_typed_errors`：受限上下文逐项探针——register_effect 未声明 key 拒绝、同 key 二次注册 `DuplicateEffect`、unregister 他人 effect 与未知 id 均 `UnknownEffect`、subscribe 未声明 requires/非 event kind 均 `UndeclaredInterface`、bound 未声明 key `UnboundInterface`；并断言 `ctx.scope()`/`ctx.manifest()` 返回宿主 scope 与自身声明快照。
- `emit_rejects_non_event_undeclared_and_unknown_emitter`：host.emit 对非 event 的已声明 key→`UndeclaredInterface`、未声明 key→`UndeclaredInterface`、未知 emitter→`UnknownPlugin`。
- `remove_descriptor_does_not_unload_running_instances`：运行中实例的声明被移除后实例继续 Active、effects 不变；下一次 `resolve` 才反映新目录（`UnknownRoot`）；声明移除不是真实卸载。
- `failed_instances_reject_being_plan_members`：Failed root 与 Failed 非 root 计划成员均 `InvalidState` 拒绝，须先 unload。
- `restart_after_stop_builds_a_fresh_object`：Stopped 后重新 start 经工厂取新对象（工厂计数递增），不复用旧对象。
- `start_reuses_active_non_root_members`：第二个 start 命中已 Active 的非 root provider→`reused` 列表，effect id 不变、服务实例同一（hits 累计）。
- `stop_subtree_records_every_deactivate_failure_in_teardown_order`：级联中多处 deactivate 失败时恢复清单按逆序逐条登记，首个错误透出。
- `rollback_records_deactivate_failures_without_replacing_the_start_error`：回滚过程中 deactivate 也失败时，恢复清单仍如实登记且 start 错误仍以激活失败方为准。
- `panicking_subscriber_fails_alone_and_delivery_continues`：订阅回调 panic→订阅者 Failed{Deliver}+恢复清单+effects 撤销；emitter 与健康订阅者不受影响继续收到投递。
- `subscribers_receive_only_their_bound_providers_events`：同 key 第二提供者激活并 emit，原订阅者（绑定第一提供者）零投递，不串台。
- `emit_inside_activate_is_allowed_and_never_backfills`：activate 内 emit 合法且无人订阅时投递数为 0。

## 门禁结果（工作树分支候选 1223f6b）

验证在独立工作树 `../nhc-d-plugin-host/NoManCode/rust-app` 直接执行（进程级 `CARGO_TARGET_DIR` 指向工作树内 `target/`，gitignore 已覆盖），不再需要冻结副本。

环境沿用联合门禁记录的本机事实并如实记录一处适配：本机仅 Windows PowerShell 5.1（`pwsh` 为 Git for Windows 内嵌 shim，不在 PATH）；VS 在非标准位置，按联合门禁记录注入 `VCToolsInstallDir`/`VSINSTALLDIR`/`WindowsSdkDir`/`UniversalCRTSdkDir`；`tests/runtime.rs` 按名称 spawn `pwsh.exe`，已将 Git 目录前置本进程 PATH。PS 5.1 在 `$ErrorActionPreference='Stop'` 下会把 cargo 的 stderr 捕获转成终止性 NativeCommandError——因此 `build.ps1 -Action X` 各步以**子进程**方式执行（stderr 走 OS 管道保持纯文本，记录真实退出码），门禁进程本身则 dot-source 由 build.ps1 第 4–73 行原样抽出的 `msvc-env.ps1` 建立同一工具链环境后直跑 cargo 定向步骤。属验证环境适配，非源码改动。

| 步骤 | 命令 | 退出码 | 说明 |
| --- | --- | --- | --- |
| env-check | `build.ps1 -Action check`（子进程，即 `cargo check --workspace --locked`） | 0 | 契约要求的环境建立步，真实执行 |
| test-integration | `cargo test -p peachsh --locked --test plugin_host` | 0 | 集成测试 **23 通过 / 0 失败**（H01–H12 与全部补充反例入口） |
| test-unit | `cargo test -p peachsh --locked --lib plugin_host::` | 0 | 模块单测 **6 通过**（53 个其他单测被过滤，未认领） |
| clippy-module | `cargo clippy -p peachsh --lib --test plugin_host --locked -- -D warnings` | 0 | 本模块零告警 |
| gate-test | `build.ps1 -Action test`（即 `cargo test --workspace --locked`） | 0 | 全 workspace 通过：lib 59（含本模块单测 6）、plugin_catalog 17、plugin_host 23、assessment_adversarial 4、final_acceptance 9、http_contract 11、runtime 10、session_turns 17、turn_http 17、peachsh_protocol 5 等全部目标；`live.rs` 1 个 `#[ignore]` 付费用例未运行 |
| gate-fmt | `build.ps1 -Action fmt`（即 `cargo fmt --all -- --check`） | 0 | 全 workspace 无 diff |
| gate-clippy | `build.ps1 -Action clippy`（即 `cargo clippy --workspace --all-targets --locked -- -D warnings`） | 0 | 全 workspace 零告警 |
| gate-wasm-check | `build.ps1 -Action wasm-check`（即 `cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked`） | 0 | UI wasm 目标编译通过 |

未执行：`build`（复制 EXE）、`live`（付费调用）。完整逐步输出见证据 `logs/`，命令/退出码/耗时见 `gate-results.json`。

复跑历史（如实记录）：开发期以 cargo check/定向 test/clippy 循环推进；契约复核后做过一轮实质修改（订阅回调 panic 隔离+归因、订阅记录绑定 provider、teardown 失败原因改经返回值传递而非嗅探恢复清单尾部、PluginFailureCause 裁至可达变体），并补全部契约规则反例测试；期间修复两个 clippy 问题（`collapsible_if`、`type_complexity`→`EventCallback`/`CtxProbe` 别名）与一个测试断言变体名（`UnknownPlugin`→`UnknownRoot`）。最终完整门禁在上表候选提交上一次性全绿；`gate-results.json` 每轮覆盖，仅末次保留。

## 契约反馈（实现期发现，未擅自改契约）

1. `HostError::PluginPanic` 按契约字面为 `{plugin_id, stage}`，无 `unwound` 字段——activate panic 时计划逆序回滚照常执行，但已回滚名单不经该错误透出（与 `ProvidesNotCovered`/`PluginStartError` 的 `unwound` 不对齐）。保留契约形状，是否补齐交由经理裁决。
2. `PluginFailureCause` 裁为三个可达变体：`PluginError{message}`（含 activate/deactivate 自报 Err 与 ctx 规则违规经由 `PluginError` 的归类）、`Panic`、`ProvidesNotCovered{missing}`；其余 HostError 子原因在 start 路径不另立 cause 变体。
3. `FailureStage` 新增 `Deliver` 变体标记订阅回调 panic 的归属阶段；契约未枚举 stage 取值集合，activate/deactivate 语义不变。
4. 订阅 effect 记录激活计划绑定的 provider_id 并按 emitter 过滤投递；契约 §3.3「provider 侧对应 effect 为 event 种类」经 key.kind==event 等价落实。
5. `ctx.emit`/`host.emit` 返回值为成功完成投递的回调数（panic 订阅者不计入）。

## 无变化声明

- 无 schema / 协议 / 权限执行变化；无新依赖（仅 std + serde/serde_json）；无文件、网络、数据库、进程、模型访问；无凭据与付费调用。
- `plugin_catalog.rs` 只读零改动；`wasm.rs`/`peachsh.wasm.v1` 未接入、行为不变；manifest 的 `permissions`/`config_schema`/`lifecycle` 仍仅记录不执行。
- 目录声明移除（`remove_descriptor`）不是真实卸载：运行实例不受影响，下一次 resolve 才反映。
- `git diff b67fedf..HEAD` 仅授权三处，工作树 HEAD 外无未提交源码改动。

## 未完成范围

- `wasm`/`process` 运行时仅声明级拒绝（`UnsupportedRuntime`），不执行；审批/capability 强制、权限授予、目录/实例持久化与重启恢复、HTTP/CLI/UI 接入均不在本轮范围。
- `pending_recovery()` 本轮只登记不自动重试；Failed 实例须 `unload` 后才能重新纳入计划。
- `peachsh.wasm.v1` 真实 Context/effect/生命周期接入属后续切片。

## 证据附件

`niuma/员工D/提交报告/第二轮/D-R2-01证据/`：

- `candidate-sha256.txt`：候选三文件 sha256 清单（与上表一致）
- `gate-results.json`：各门禁步骤命令/退出码/耗时（最终 run）
- `logs/`：env-check / test-integration / test-unit / clippy-module / gate-test / gate-fmt / gate-clippy / gate-wasm-check 完整输出
- `run-gate.ps1`：门禁驱动脚本副本（含 PS 5.1 子进程方案与环境适配注释）
- `msvc-env.ps1`：由 build.ps1 第 4–73 行抽出的工具链环境块（run-gate.ps1 每次运行自动重建）

报告提交不等于经理验收或 Git 整合；工作树分支上的提交 `1223f6b80591eb5c54e480f09991f67e525e3b08` 为本人交付候选。
