# NEXT-20 默认能力组合：代码盘点与下一契约建议

2026-11-01，员工B / Codex 接续。只读主仓 `main` HEAD `cfaa7a0e3962e42b934b0ef7923358f6e1caea41`，NoManCode tree `b02d06979681a7c5ee45093b871046996c5f5fb2`、Rust tree `9d6b09f50d0925f0644f59fbc048548ec6f5dcab`；本人核产品 diff/index 空。历史已验收结果不在本轮重跑，以下是现行代码事实及建议，不是实现或验收。

## 决策建议

**选统一 typed 组合根 + 现有四个独立 runtime，不将13项塞进一个 PluginHost 大锁。** `Engine::new`/`assemble` 已是事实装配根（`engine.rs:785,867`），下一步应让它成为不可伪造的真实依赖入口：组合真正调用的 Host 服务和四个现有 runtime，生产业务从组合后的具体句柄调用，缺少必需依赖构造失败，不能悄悄替代、自动启用或降级。不要为凑数量创建仅返回名字的13个空插件，也不要把 SQLite、审批、恢复抽成可绕开权限的开关。

SQLite/Store 是持久事实权威，进程内 gate/active/lease 是并发与运行态，不是第二持久权威；外部 workspace/Git 内容及其身份是受验证的外部事实。`main.rs:107-126` 当前正常启动先 open/初始化、`recover()` 再 `Engine::new`，`--check` 路径提前退出，不能在重构中意外重复恢复或改变 check 语义。core、session-log、project 身份/权限、approval 最终准入、workspace 安全验证和 recovery 都由 Host 持有、随 Host 生灭，不提供热卸载；策略/codec/tool 实现才是可替换边界。

## 13 项真实调用盘点

下表「缺口」指组合和可替换边界，不等于功能不存在。路径均相对 `NoManCode/rust-app/src/`；生命周期列写当前实际行为。

| 项目书能力 | 真实服务与调用、权威、当前生命周期 / 缺口 |
| --- | --- |
| core | `main.rs:107` 建 Store 并恢复，`engine.rs:757,785,867` 装配 Engine、gate、active、semaphore；SQLite 配置/运行事实，gate/计数仅进程内。Host 生命周期内必需；缺明确 typed 组合所有权及失败/关闭契约。 |
| session-log | `repository.rs` 的 session/turn/task/event、`store.rs:974,1736`，Engine 运行与 `server.rs:2657,2685` 事件流调用；SQLite 状态/seq 为权威。Host 常驻、不可单独停；直连 Store 分散，缺可替换存储契约与统一依赖入口。 |
| local-project | `project_config.rs:247,280,380,457,486` 注册、选中、配置与 `task_project_bindings`；Engine 入口 `engine.rs:218`，HTTP/CLI 为调用方。SQLite revision/身份绑定及真实 root 身份共同约束；不可按插件停用；旧未绑定项目有明确拒绝，缺 typed project authority。 |
| workspace | `workspace_identity.rs:90,112,147`、`workspace.rs:106,155` 验证路径/root，Engine gate 串行写/恢复/部分操作；磁盘与项目绑定为事实，非内存目录清单。随 Host 运行，不是独立已注册插件；缺可共享的安全能力边界。 |
| provider-xpeach | `provider_runtime.rs:169,217,254,269` codec/decoder；`engine.rs:1979` 联合 `network_authorization.rs:905`、`provider_transport.rs` 网络/预算准入。SQLite desired/网络授权/receipt 权威，进程 host/generation/lease 表示实际状态；可停的当前实现是 builtin，生产替换/选择接口未开放。 |
| file-read-search | `ToolRuntime` 的 `builtin.files` 提供 read/list/search，`tool_call_application.rs:153` allow → `ToolRuntime::acquire` → `Store::validate_files_read` → `workspace.rs:218`。项目绑定、任务权限、SQLite desired 与实时磁盘约束；该组关闭阻止新准入，已准入可结束。与 file-write 共享一个 files 组，不能单独开关/替换。 |
| approval-policy | `approval.rs:170,179` classify/evaluate，`tool_call_application.rs:153` 及 `store.rs:1788,1921,2084` 创建/决定/claim，绑定 scope、调用、摘要和事务；SQLite 审批/receipt 为权威。Host 强制最终准入不可停，Agent vote 只能更严格；缺可注入且不能绕开此守门的策略契约。 |
| file-write | 同 `builtin.files`，`tool_call_application.rs:293` 在 gate 内 project 验证、审批 claim 后 preflight、`prepare_workspace_change`、执行、`finish_approval_with_change`；DB before/after/receipt 与真实磁盘共同决定结果。停 files 不撤销已 claim 的 lease，未知结果须恢复/人工检查；不能把普通工具调用当唯一事务。 |
| shell | `builtin.shell` → `workspace.rs:340` → `command_execution.rs`；需任务 commands、项目权限、审批及网络风险确认；DB 审批/执行状态与真实子进程共同决定结果。独立于 files 管理，新准入关闭后已准入可完成；不是进程沙箱，未确认结果保持 unknown。 |
| git-diff | `engine.rs:6782` 下 `git_diff` 持 gate、校验绑定/活动任务，再 `git_diff.rs:518` 受限 Git 读取；Git HEAD/index/工作树为事实。非插件、不可单独停；缺显式受限只读服务依赖，避免把工具禁用当成禁止审阅。 |
| checkpoint-restore | `engine.rs:6828,6958`、`store.rs:3881` 及 `workspace_changes.rs` 读变更、checkpoint/restore receipt；gate、磁盘散列、事务和 SQLite 共同裁决。Host 安全能力不应随 files/shell 停用，未知/部分不重放；缺清晰组合句柄和独立能力证明。 |
| recovery | `main.rs:125` → `Store::recover`（`store.rs:2256`）及后续 resume/reconcile；SQLite 任务、已 claim 网络/审批/工作树/恢复状态被保守标记。Host 启动必需，不自动重做副作用；不是 PluginHost 的内存 effect recovery，同名不可混淆。 |
| chat-agent | `agent_runtime.rs:83,212,307` 的 builtin strategy，`engine.rs:1884` acquire 后协调 provider、tools 与 Store；SQLite desired/任务事实，进程 AgentLease/generation。可关闭新准入，在途排空后停止；当前替换策略注入仅测试可用，Host 审批与 effect 不属于 Agent 权力。 |

补充：`workspace::definitions` 包含 `run_wasm` 兼容定义，但实际 `tool_call_application.rs:164` 显式拒绝该项目操作；不能把展示出来的定义当作可执行能力，也不属于本轮13项通过证明。

## 四个生命周期域的实际后果

`ToolRuntime` 内 files 与 shell 各有一个 `PluginHost`/mutex，Provider、Agent 各自一个，总共**四个 host 图**（`tool_runtime.rs:462-540`、`provider_runtime.rs:169`、`agent_runtime.rs:212`）。每个图只注册自己的 builtin provider 和 engine consumer；`plugin_catalog.rs:25,936` 的依赖解析只在该 catalog 内，接口版本是精确三段相等（不是版本区间），不存在跨四图的全局冲突检测或自动停止顺序。files 插件版本1.2.0、shell 1.0.0，实际工具接口1.0.0；agent 与 codec 接口各1.0.0，插件版本与接口版本不可混为一谈。

`plugin_management.rs:75-101` 四项共用 SQLite `plugin_desired` 与请求收据/idempotency key；不是四套持久权威。但 HTTP/CLI 管理有两套既有契约：`/api/plugins` 仅 files/shell（`engine.rs:1235`），`/api/runtime/components` 仅 agent/provider（`engine.rs:106`）；CLI `cli.rs:2317` 对旧列表/版本有严格校验，直接往旧列表塞新项会破坏客户端。状态先后观察，不是跨四域原子快照。

files/shell 以各自锁内短同步准入/Store事务和 ToolLease 隔离：停用关新入口，已准入 lease 可完成，状态能呈 draining；审批等待不持 lease。Agent 在锁内检查持久 desired 并计数，禁用后旧任务 lease 排空再 stop，排空时重启可 Busy（`agent_runtime.rs:307,508,584`）。Provider 先 prepare 并绑定 effect/generation/desired revision，最终在 runtime 锁内由网络准入事务复核 revision 与预算，关停使旧 prepare 失效，已准入 lease 可完成（`provider_runtime.rs:254-325`、`engine.rs:1979`）；与 Agent 排空策略不同。运行管理确认的是持久期望已提交，执行 apply 可能失败/未收敛；重启从 DB desired 初始化，禁用项不应激活。旧 Rust in-memory disable 入口不持久，不能改成持久命令。

**比较：**四域继续独立可保持现有 runtime→Store 锁序、隔离坏插件及 in-flight 语义；typed 根负责编译期装配和跨域依赖校验，不在长操作/await 期间持组合锁。单 PluginHost 可以统一本地拓扑及反向停用，但当前不能表达跨网络事务、审批 claim、隔离锁和四种 lease 生命周期；迁移会把可能慢/恐慌的激活、析构与互相独立的管理操作绑到大锁，增加死锁、重入和停用扩散风险。仅有多个 registry 并非缺陷；真正缺口是跨域关系未成为生产调用的强类型约束、可审计身份和一致错误语义。

## 建议 NEXT-20 一次交付契约（待经理裁决）

1. **真实组合与所有权。** 增新 `composition.rs`（名称可由经理定）定义 Host 私有 `HostComposition`、必需 Host 能力的具体 typed 句柄，以及现有 files/shell、Provider、Agent 四运行域；组合构造只接收已打开的同一个 `Arc<Store>` 和已验证的 Host 依赖。`Engine::new/assemble` 改用此根，`ToolCallApplication::new` 接收所需权限/结果/执行句柄；用类型字段与构造器连接13项映射中的真实实现，实际调用这些句柄的方法，禁止只存标签、空 trait 或一套旁路实现。禁止组合根复制 desired/receipt/权限表或允许插件持裸 Store/审批写权。
2. **业务接线必须真实。** 在 `engine.rs`、`tool_call_application.rs`、`tool_runtime.rs`、`agent_runtime.rs`、`provider_runtime.rs` 及必要的现有 Host 模块中，覆盖正常启动/recover 后装配、project/权限、Agent → provider completion → ToolCall → read/write/shell → 事件/审批、git diff、checkpoint/restore、resume/新 Turn 的现有生产入口；本轮逐项注明实现到具体调用点。`server.rs` 现存部分直接使用 `engine.store`（如 `:576,1192,2685`），有关13项的生产 API 须使用同一组合持有的 Host 服务，不能让 HTTP/CLI 仍走旁路；Rust 兼容的 `Engine.store` 若保留只能是同一 Arc 的兼容别名，不作为第二数据源。`main.rs` 不重复 recover；`--check` 不假装已经装配。
3. **生命周期与失败。** Core/approval/recovery/root 安全服务无 disable/unload API；缺失或身份不一致启动/操作 fail closed。四域保留各自现有 desired、锁、generation、lease、事务顺序和关闭语义；某域 disabled 不令全部 Host 启动失败，也不得为它提供 builtin fallback。销毁顺序从请求准入到 consumer 再 provider，保留当前在途语义与未知状态，不要求把四域虚构成可原子提交的整体；激活中途失败只清理本轮已创建资源，不回滚已提交的 desired。不得持组合锁调用其他 runtime/Store 或跨 await，维持既有 gate 与 runtime→Store 顺序。初始化/关闭、必需依赖缺失、已提交但未收敛、外部事实未知均有可区分错误。
4. **明确版本与身份。** typed 句柄版本作为编译期契约；四既有 `InterfaceKey`/manifest 各自 exact 版本照旧，启动时匹配真实 active binding，拒绝类型/版本不匹配或重复有歧义的绑定；当前不实现动态外部插件选择、跨进程卸载或虚构全局 resolver。替代实现可先仅通过测试工厂验证，但必须真的接到对应生产调用链；生产可安装/升级需要后续独立契约。
5. **兼容与观察。** 不改 schema17、四条 desired/receipt、idempotency 与原 API/CLI DTO/列表，旧数据库升级、现存任务/审批/恢复结果及兼容 Rust 控制语义不变。若需要公开组合详情，增加独立只读版本化 API/CLI，而非改 `/api/plugins` 既有严格列表；返回实际绑定、禁用/不可用、独立快照时间及来源，不冒称跨域原子快照，不泄漏密钥或原参数。建议本次交付一个真实只读组合检查供诊断，但验收以业务调用和反例为主，不以清单数量作为完成。
6. **唯一写入人和边界。** 经理先冻结本契约、main/双tree、新分支/工作树、源文件唯一 owner、各员工任务和串行整合。候选生产范围为 `composition.rs`、`lib.rs`、`engine.rs`、`tool_call_application.rs`、四 runtime 所在文件及必要 Host 实现；`main.rs`/`server.rs`/`cli.rs` 仅就实际接线和可选新增只读接口修改，Store/Repository 不改 schema 和审批逻辑，仅允许必要类型化委托；共享测试各有唯一 owner。任何 schema、外部 API 破坏、通用插件安装或新持久状态需另行经理裁决。

## 一次交付验证矩阵

- **组合真实性与失败**：固定13项到实际方法的追踪；每个必需依赖缺失、错误类型/版本、重复绑定或激活失败均 fail closed、无副作用；测试替身只由显式注入路径计数且替换确实改变对应真实调用，默认路径不 fallback。`--check`、首次启动、异常启动和清理顺序可见。
- **权限与恢复不可绕过**：fake Agent/tool/provider 尝试扩大 tools、写 scope、跳过 approval/网络/预算、注入事件或访问凭据均被 Host 拦截；审批拒绝/取消、等待停用、claim 竞争、unknown、进程崩溃及重启、重复恢复/新 Turn 保留原记录，不重复副作用。checkpoint/git-diff 不依赖 files/shell 的开关。
- **四域时序**：各自开关、旧 intent/replay/CAS、同/跨 Store 禁用与准入交错，已 admitted lease 可完成、未 admitted 不启动；Agent 排空/重启 Busy、Provider prepare 的 generation/revision 失效、files/shell 的独立停用，隔离失败/锁外析构、死锁及最终状态。快照是非原子 observation，不能把期望写成已生效。
- **持久兼容与公开链**：schema17 旧库/历史任务、四条 ledger/receipt/hash/幂等/重启、旧 HTTP/CLI 严格列表、错误及 JSON 字段回归；若加新接口验证无秘密、版本/未知状态和旧客户端不变。以本地 mock 完整走 create project → Chat → read → 审批 write/command → diff/checkpoint/restore → restart/new Turn，并在真实 HTTP/CLI 做无副作用及故障分支；单元、跨 Store、真实进程崩溃及最终 workspace/all-targets/wasm 门禁分层且首失败停止。

这可一次交付**一个真实默认组合内核**，仍不等于13项均变成第三方插件：外部插件安装/签名与版本迁移、跨 scope resolver、可持久选择替代实现、Desktop、跨机器同步与完整产品阶段验收另立后续任务，不以本轮默认组合代替。以上为建议，经理决定产品语义与分工；本人未改产品或运行测试。
