# D-R2-01：插件 Host 注册表与可撤销生命周期（builtin）

日期：2026-09-23；修订1；全局切片NEXT-02B；项目NoHumanCode，根 `./`，源码 `NoManCode/`，Rust目录 `NoManCode/rust-app`。

**执行角色：员工D；执行模型由用户分发时确定。** 上轮 D-R1-01 用户指定 SWE2max，本任务建议延续同一执行线以消费 D 自有目录 API；用户未另行指定时按默认安排。经理负责契约与review，员工负责实现、调试、正式测试；用户分发提示词。状态：经理已准备、待用户分发，尚无执行标识，不宣称已开工。

来源：任务板"NEXT-02后续"与[项目书](../../../../PROJECT-BOOK.md) §3.1～3.4、§5、§10（目录计划与 WASM 沙箱均不算完整宿主）。本任务消费已验收并入库的 NEXT-02A `plugin_catalog`（基线 `b67fedf`）。收到用户交付本有效包后，D 登记来源/接手时间/执行标识即可工作，不重复要求形式确认。

## 1. 交付能力与边界

实现真实可调用、可测试的纯内存插件 Host 运行时：持有一个 `PluginCatalog` → `start(roots)` 当场 resolve 得确定性计划 → 按拓扑序实例化并激活 `builtin` 插件（经调用方注入的工厂）→ 插件在 `activate` 中经 `PluginContext` 注册其已声明 provides 的 effect、按 bindings 消费 requires → `stop`/`stop_subtree`/`unload` 先消费者后提供者地撤销全部 effect → 激活失败按计划逆序回滚已激活实例；插件自报清理失败进显式恢复清单。

依据：项目书 §3.1（Host 管上下文/注册表/生命周期）、§3.2（effect 可追踪、卸载撤销、不可撤销走显式恢复）、§5 验收（禁用/卸载后不残留注册项、监听器和后台任务）。`plugin_catalog.rs` 是只读依赖，本任务不改它；`wasm.rs` 的 `peachsh.wasm.v1` 独立沙箱保持兼容，不接入、不自动导入。

明确不做：`wasm`/`process` 运行时执行（启动即 typed `UnsupportedRuntime`）、审批/capability 强制、权限授予、持久化/重启恢复、HTTP/CLI/UI、schema 变更、凭据/付费调用、异步任务。Context 是 activate/deactivate 期间的受限句柄，不是全局服务定位器；manifest 的 `permissions`/`config_schema` 仍只记录不执行。本轮输出是真生命周期与可撤销 effect，不是外壳状态机——activate/deactivate 必须真实调用插件对象并回收注册项。

## 2. 基线、Git与唯一所有权

| 项目 | 约定 |
| --- | --- |
| Git 模式 | **独立工作树**（[Git 协作规范](../../../../Git协作规范.md) §2/§3；首个登记基线后的第一个独立任务） |
| 基线完整 SHA | `b67fedfcd6ca35969363096ea64ddd5fb1290524` |
| 分支 / 工作树 | `codex/d/plugin-host` / `../nhc-d-plugin-host/`（仓库根旁的同级目录；经理已创建并核对 HEAD=基线） |
| 提交执行人 | 员工D在任务分支提交本人范围内的小步修改；共享树 `./` 仍由经理唯一 Git 写入 |
| 整合人 | 经理：审查候选 SHA 后按 §6 串行整合到 `main` |
| 报告与身份 | 写共享树 `./niuma/员工D/…`；不编辑工作树内的 niuma 副本，工作树内只提交源码三处 |

唯一源码写入人为D，限定以下三处（相对工作树 `NoManCode/rust-app/`）：

1. 新增 `src/plugin_host.rs`：本轮全部类型、上下文、注册表、状态机、算法，必要内部单测。
2. 新增 `tests/plugin_host.rs`：从公开API验证完整链路与反例。
3. `src/lib.rs`：仅追加 `pub mod plugin_host;` 一行，不改其他导出。

D 可更新自己在共享树的第二轮报告、证据及个人身份。任务与review由经理维护。不得修改 `plugin_catalog.rs`（发现 API 缺口写进报告由经理修订）、`wasm.rs`、`server.rs`/`turn_http.rs`、engine/store/repository/domain/secrets/provider/workspace、crates/、Cargo.toml/Cargo.lock、脚本、既有测试、web/ 及他人 niuma 档案。std + 现有 serde/serde_json 足够：effect 负载用 `std::any`/`Box<dyn …>`，不新增依赖。新模块写完整后才接 lib 导出；需要拆文件或新依赖先由经理修订。

## 3. 数据契约

语义固定，Rust 签名可选借用/owned 以保持实现清晰，但须提供下列可被集成测试调用的入口。

### 3.1 宿主与目录

- `PluginHost::new(scope: ScopeKey) -> Result<PluginHost, HostError>`：按 catalog 同规则校验 scope 身份，内部创建空 `PluginCatalog`。一个 host 绑一个 scope；两个 Project/Session host 互不满足，禁止全局注册表或父作用域回退。
- `host.catalog()` / `host.catalog_mut()`：访问目录；`register`/`remove_descriptor` 沿用 catalog 语义。目录变更不自动 unload 实例；`remove_descriptor` 移除运行中插件的声明是声明级操作，实例继续运行，下一次 start 的 resolve 才反映新目录。
- `host.resolve(roots)` 或等价委托：返回 catalog 当前快照的计划，供调用方/测试检查，不执行。
- `start(&mut self, roots: &[String]) -> Result<StartReport, HostError>`：先 `catalog.resolve(roots)`（`CatalogError` 保留证据透传），再按 `ordered_plugins` 顺序处理每个 `PluginRef`：无实例或 Stopped → 经工厂取新对象、Loaded→Active；已 Active 的非 root → 复用不重复激活；已 Active 的 root → `InvalidState`；`Failed` → `InvalidState`，须先 unload。返回按激活顺序的 `PluginRef` 列表与复用列表。

### 3.2 执行体（仅 builtin）

- `BuiltinPlugin` trait（`Send`）：`activate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError>` 与 `deactivate(&mut self, ctx: &mut PluginContext) -> Result<(), PluginError>`；`PluginError` 是插件侧 typed 错误。
- 工厂经 `with_factory(id, factory)` 或构造注入，按插件稳定 id 查找；`runtime == builtin` 且无工厂 → `MissingFactory`；`runtime` 为 wasm/process → `UnsupportedRuntime`。两种情况都不实例化、不激活。
- activate/deactivate 调用以 `std::panic::catch_unwind` 隔离；panic 记 `PluginPanic`，实例 `Failed`。
- 每次 Loaded→Active 都经工厂取新对象；Stopped 后旧对象已释放，重新激活不复用旧对象。

### 3.3 PluginContext（仅在 activate/deactivate 调用期间有效）

- `ctx.plugin_id()` / `ctx.scope()` / `ctx.manifest()`：自身稳定 id、scope、自身声明快照。
- `ctx.bound(key) -> Result<BoundInterface, HostError>`：仅允许 `key ∈ manifest.requires` 且当前计划 bindings 存在 consumer==自身 的该 key 绑定；返回 provider 已注册 effect 的只读访问（`&dyn Any` 或等价守卫），由消费方 downcast。未声明 requires 或无绑定 → `UnboundInterface`。不提供全局或按显示名查询。
- `ctx.register_effect(key, payload) -> Result<EffectId, HostError>`：仅允许 `key ∈ manifest.provides`；`UndeclaredInterface` 拒绝未声明键；同一插件同一 key 重复注册 → `DuplicateEffect`。`EffectId` 确定递增。payload 容纳服务/命令/查询/资源/能力对象（`Box<dyn Any + Send>` 或等价）。
- `ctx.unregister_effect(effect_id)`：仅注销自身 effect；宿主在 deactivate 结束后强制回收全部残留，见 3.5。
- `ctx.subscribe(key, callback) -> Result<EffectId, HostError>`：`key ∈ manifest.requires`、已绑定且 provider 侧对应 effect 为 event 种类；生成归属 consumer 的订阅 effect。`ctx.emit(key, value) -> Result<usize, HostError>`：`key ∈ manifest.provides` 且 kind==event，按订阅者插件 id 升序同步投递，返回投递数；订阅发生于 emit 之后的不得补投。
- activate 返回前宿主校验该插件 manifest.provides 的每个 key 都已有自注册 effect，缺失 → `ProvidesNotCovered{missing}` 并回滚；不要求 subscribe/coverage 对 requires 全消费。

### 3.4 effect 注册表与检查面

- `EffectRecord{effect_id, plugin_id, key: InterfaceKey}`；注册表按 (kind,name,version) 索引，所有快照/清单按确定顺序返回。
- `host.instances()`：按 id 排序的 `InstanceSnapshot{id,version,state,effect_ids}`；`host.effects()`：effect 快照；`host.pending_recovery()`：恢复清单。三者只读、确定序、不含 payload 内部细节。

### 3.5 生命周期状态机

`Registered → Loaded → Active → Stopped → Unloaded`；`Failed{stage}` 在 activate/deactivate/unwind 失败时进入。

- `stop(id)`：该插件有 Active 消费者（按激活期 bindings）→ `DependentsActive{dependents}`，依赖者按 id 排序、状态不变。否则 deactivate（catch_unwind）→ 宿主强制回收其全部 effect → `Stopped`；deactivate 返回 Err 或 panic → `Failed` 且恢复清单登记（见 3.6），注册表侧仍保证无残留。
- `stop_subtree(id)`：把 Active 依赖者一起按激活逆序停掉后停自身；语义同 stop。
- `unload(id)`：仅 Loaded/Stopped/Failed 可卸载；移除实例记录与其残留 effect（Failed 的残留转恢复清单）；`Active` → `InvalidState`。unload 不删目录声明，删声明走 `catalog.remove_descriptor` 且不影响实例。
- 非法迁移一律 `InvalidState{plugin_id,current}`；未知 id → `UnknownPlugin{id}`。display_name 不参与身份、排序或依赖。

### 3.6 错误与显式恢复

`HostError` 至少含稳定变体：`Catalog(CatalogError)`、`UnsupportedRuntime`、`MissingFactory`、`UnboundInterface`、`UndeclaredInterface`、`DuplicateEffect`、`ProvidesNotCovered{missing}`、`DependentsActive{dependents}`、`InvalidState{plugin_id,current}`、`UnknownPlugin`、`PluginStartError{plugin_id,stage,cause,unwound}`、`PluginDeactivateError{plugin_id}`、`PluginPanic{plugin_id,stage}`。`start` 失败：失败实例 `Failed`，已 Active 实例按计划逆序 deactivate+回收，`unwound` 列实际回滚的 `PluginRef`；`cause` 携带 typed 子原因（插件 Err/panic/ProvidesNotCovered/UndeclaredInterface 等）。

错误证据含插件稳定 id、InterfaceKey、当前状态；不 echo `config_schema`、凭据或原始输入字节。`pending_recovery()` 返回确定序的 `RecoveryItem{plugin_id,stage,cause}`：deactivate 失败/panic 或 unload 时 Failed 残留的登记项，表示插件自报清理未完成、需人工/后续切片处理；本轮只登记不自动重试。注册表侧 effect 始终被回收，恢复清单不表示注册表有残留。

### 3.7 确定性与纯度

BTreeMap/排序遍历；相同输入产生相同激活与撤销顺序；无文件/网络/数据库/进程/时钟/环境变量读取（payload 内部行为是插件自身责任，host 本体不 IO）。不把 permissions/config_schema 用作操作指令；不需要真实凭据或付费模型。

## 4. 必须验证的场景

每项报告给测试名、断言、实际结果；测试在 `tests/plugin_host.rs` 使用公开API，内置最小假插件（计数器服务、录事件、可控失败/ panic 夹具）。

| 编号 | 反例或场景 | 必须断言 |
| --- | --- | --- |
| H01 完整链 | provider+consumer 两 builtin，register→start | 激活顺序 provider→consumer；consumer 经 `ctx.bound` 取得真实 payload 并成功调用；instances/effects 快照完整且确定序 |
| H02 拓扑序 | 链、菱形、多 roots | 激活序=计划序；`stop_subtree` 逆序=消费者先于提供者；与 roots/注册顺序无关 |
| H03 撤销 | stop_subtree(provider) 后 | provider 全部 effect 消失、订阅停止、consumer 已停且其订阅移除；`instances`/`effects` 无残留；unload 后实例消失 |
| H04 依赖保护 | 直接 stop 有 Active 消费者的 provider | `DependentsActive` 列按 id 排序的依赖者；双方状态不变 |
| H05 激活失败 | consumer activate 返回 Err；另一例 panic | `PluginStartError`/`PluginPanic` 含 id 与 stage；已激活 provider 被逆序回滚、effect 无残留；`unwound` 准确 |
| H06 声明违背 | 注册未声明 provides 的 key；漏注册已声明 provides | `UndeclaredInterface` / `ProvidesNotCovered{missing}` 准确；整体回滚无残留 |
| H07 运行时 | wasm/process 声明参与计划；builtin 无工厂 | `UnsupportedRuntime` / `MissingFactory`；不实例化、无 effect、无调用 |
| H08 状态与作用域 | 重复 start 同 root、stop 已停、unload Active、未知 id、两个同 kind scope 同名能力 | `InvalidState`/`UnknownPlugin`；不同 host 实例互不满足、互不影响 |
| H09 事件 | provider event sink+consumer subscribe→emit→双方停 | 按订阅者 id 序确定性投递、计数正确；provider 停后无投递；consumer 停后其订阅移除且 emit 不再投给它 |
| H10 恢复清单 | deactivate 返回 Err/panic 留残 | 实例 `Failed`；注册表无残留；`pending_recovery` 按序登记；unload(Failed) 后仍登记且实例移除 |
| H11 纯度/身份 | host.resolve 重复调用；bound 未绑定 key；同 display_name 不同 id | 计划一致；`UnboundInterface`；display_name 不构成身份 |
| H12 回归 | 工作树内既有门禁 | test/fmt/clippy/wasm-check 全绿；`git diff b67fedf` 只含授权三处；catalog/wasm/其他源码零改动 |

## 5. 验证与交付

独立工作树即隔离环境：在 `../nhc-d-plugin-host/NoManCode/rust-app` 直接跑门禁，不再需要 `.local/verification` 冻结副本；工作树内 `target/` 被忽略，首次构建为全新编译。报告/身份写共享树 `./niuma/员工D/…`；共享树 `./` 的暂存与提交仍只有经理执行，D 在工作树分支自行提交源码三处的小步修改。

环境沿用 [联合门禁记录](../../../项目经理/审查记录/2026-09-23C-D联合门禁.md) 的本机事实：pwsh 为 Git 内嵌 shim（PS 5.1）、VS 非标准位置需显式注入 `VCToolsInstallDir`/`VSINSTALLDIR`/`WindowsSdkDir`、Git 目录前置 PATH。同一 pwsh 进程先 `& ./build.ps1 -Action check` 建环境（记录退出码），再运行：

```powershell
cargo test -p peachsh --locked --test plugin_host
cargo test -p peachsh --locked --lib plugin_host::
cargo clippy -p peachsh --lib --test plugin_host --locked -- -D warnings
```

交付前在工作树对当前分支候选完整执行 `build.ps1 -Action test`、`-Action fmt`、`-Action clippy`、`-Action wasm-check`（fmt 仅检查）；不执行 build（复制 EXE）、不执行 live（付费）。`git diff` 只含授权三处；共享树另查报告/身份文件。

报告：`niuma/员工D/提交报告/第二轮/员工D（插件Host注册表与生命周期 第1.0轮报告）.md`；有限脱敏附件同目录 `D-R2-01证据/`（相对路径，不写本机绝对路径）。经理review目标：`niuma/员工D/审查记录/第二轮/员工D review 1.0.md`，当前不存在即未审查。报告记录实际执行模型、任务执行标识、分支 `codex/d/plugin-host` 与候选提交完整 SHA、H01～H12 映射、失败/未执行项、范围与后续限制；更新本人身份，交回报告路径。报告提交、Git 提交、验收、合并、发布各自记录；员工在工作树分支的提交不等于经理验收或整合。
