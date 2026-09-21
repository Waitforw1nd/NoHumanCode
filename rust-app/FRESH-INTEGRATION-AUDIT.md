# Peachsh Rust 集成基线审阅（2026-09-22）

本文件是从源码和可运行入口重新建立的基线；仓库内既有审计、路线图和 README 的结论只作为线索，未当作事实。证据优先级为 `src/`、`web/`、测试和根目录启动文件。本文只记录审阅结果，不修改业务代码。

## 1. 产品定位

当前产品是一个 Windows 本机运行的、面向项目目录的模型任务工作台：用户配置 OpenAI Chat Completions 兼容路由和 Key，直接进行单模型对话，或提交带固定身份、职责、模型路由、依赖关系和写入范围的多个任务；Rust 服务负责调度、持久化、流式事件、项目工具和可选 WASM 工具。HTTP 路由在 `src/server.rs:39-59`，核心聚合在 `src/engine.rs`，网页是内嵌的 `web/index.html`、`web/app.js`。

它不是旧 DeepSeek Harness 的完整插件生态移植：本次审阅没有在 Rust 源码中发现旧版会话、MCP/Skill/插件调用或跨主机协作的对应实现。根目录 `README.md` 仍保留旧 Node 启动、配置和团队概念，故不能把根目录描述直接视为 Rust 产品契约。

## 2. 从源码还原的真实用户流程

1. 启动 `rust-app` 的 Rust 可执行程序；`src/main.rs` 创建数据目录、SQLite `Store`、`Engine` 和 Axum 服务，并在退出时取消活动任务。首次设置由 `store::initial_settings` 从旧配置导入（`src/store.rs:300`），之后配置存于独立 SQLite。
2. 浏览器请求 `/api/bootstrap`、`/api/settings`。设置页可保存工作目录、全局并发、路由（地址、模型、并发、Key 环境变量）以及 New API 账号；保存入口是 `PUT /api/settings`（`src/server.rs:46`）。Key 进入 `secrets` 表并由 `secrets.rs` 封装，GET 设置只返回 Key 状态而不是值（`web/app.js` 的 `loadSettings`/`renderRoute`）。
3. 用户可在“新对话”中选择一个路由、选择是否启用读取/写入/命令权限，提交消息；前端把它转成名为 `chat-session` 的单任务。工作台则提交 `RunRequest { title, tasks }` 到 `POST /api/runs`。`TaskSpec` 的校验包括名称、依赖无环、路由存在、范围规范化和不相交写入范围（`src/domain.rs:197-263`）。
4. `Engine::start_locked` 为每个任务保存路由快照、系统身份提示和初始消息，创建 Run/Task 后异步启动（`src/engine.rs:153-225`）。依赖任务完成后才会释放下游；全局 Semaphore 和按 Key 的 Semaphore 限制并发（`src/engine.rs:269-320`）。模型调用由 `provider::complete` 处理 SSE 和工具调用（`src/provider.rs:62-177`）。
5. 任务工具由 `workspace::definitions/execute` 暴露：列目录、读文件、写文件、PowerShell 命令以及 `run_wasm`（`src/workspace.rs:71-105`）。写文件按范围校验并记录备份；命令权限不受写入范围限制，因此源码明确要求命令任务单独运行。
6. 前端订阅 `/api/runs/{id}/events` 的 SSE，按 `delta`、工具和状态事件实时刷新（`web/app.js` 的 `openRun`）。事件写入 SQLite `events` 表并支持游标（`src/store.rs:212-249`）；刷新重连可回放持久事件。
7. 用户可停止任务、继续任务、查看已记录变更、恢复备份：分别是 `/api/tasks/{id}/cancel`、`resume`、`changes`、`restore`（`src/server.rs:56-59`；`src/engine.rs:421-516`）。进程启动时 `Store::recover` 将未完成任务标为中断并恢复可见输出（`src/store.rs:256-298`）。

## 3. 已实现与缺失（以源码为准）

已实现：

- 本机 HTTP UI、设置、模型发现和连通性检查；路由及接口清单见 `src/server.rs:41-59`。
- 单任务连续对话和多任务 Run；任务身份由 `TaskSpec.name/role` 固定，路由快照保存在 `Task`（`src/domain.rs:274-303`）。
- 依赖 DAG、循环检测、写入范围冲突、全局/按 Key 限流、取消和恢复。
- OpenAI 兼容 Chat Completions 的流式 UTF-8/SSE 解析与 tool-call 拼接（`src/provider.rs:66-160`）。
- 文件读写、备份/恢复、命令执行；命令是当前用户权限，最长参数可到 600 秒（`src/workspace.rs:96-101`）。
- SQLite WAL、任务/事件/idempotency 表（`src/store.rs:13-32`）；Windows DPAPI 分支在 `src/secrets.rs`。
- WASM 插件目录加载、manifest 校验、SHA-256、无 host imports、内存/fuel/输入输出限制；ABI 常量为 `peachsh.wasm.v1`（`src/wasm.rs:13-18, 88-231`）。

缺失或尚未能由源码证明：

- 没有发现自动模型故障切换、重试策略、上下文压缩、任务删除/归档、幂等键过期清理或后台重启；`idempotency` 只有按键查询/插入（`src/store.rs:118-161`）。
- 没有发现旧版会话历史、MCP/Skill/外部插件协议、跨主机协作、工作树隔离或多项目选择器的 Rust API。
- 没有独立的领域层/应用层/基础设施端口；`Engine` 直接持有 `Store`、`reqwest::Client` 并调用 `workspace`，UI DTO 与持久化 `Task` 也高度重合。这会使协议迁移、测试替换和未来桌面壳集成变难。
- 认证是本机请求令牌/同源边界，而非用户/租户身份系统；源码未显示远程部署的授权模型。命令执行能力仍是实质性的操作系统权限。
- 当前协议只接受 OpenAI Chat Completions 形态；没有 provider capability 抽象，模型发现、余额和完成请求的差异由 `provider.rs` 直接编码。

## 4. 推荐领域模型

建议把“用户意图”和“运行时事实”分开，保持存储可迁移：

| 聚合/实体 | 关键字段与不变量 |
|---|---|
| `Project` | canonical root、显示名、工具策略；所有路径相对于 root，禁止联接/设备名/越界。 |
| `ProviderRoute` | id、endpoint、model、token policy、parallel limit；Key 引用凭据库，不进入 JSON/消息。 |
| `Conversation` | 标题、route snapshot、消息、状态；单用户连续对话。 |
| `Run` | idempotency key、标题、kind、created/finished 状态；拥有 DAG 中的 `WorkItem`。 |
| `WorkItem` | immutable identity/name/role、prompt、dependencies、capabilities、write scopes、route snapshot、attempts。 |
| `Execution` | 每次尝试的状态、取消原因、provider usage、错误、开始/结束时间。 |
| `ArtifactChange` | task/execution、相对路径、原内容引用、写入结果、restore 状态；事件只做审计，不承担当前状态。 |
| `EventStream` | 单调 seq、事件类型、payload、schema version；SSE 只是投影。 |
| `WasmPlugin` | id、ABI、manifest、digest、limits、启用状态；宿主执行接口独立于 agent tool JSON。 |

应用服务可以拆为 `ConfigureProject`、`StartRun`、`ResumeWorkItem`、`CancelExecution`、`RestoreChanges`、`SubscribeRunEvents`；基础设施端口再分别实现 `RunRepository`、`SecretStore`、`ModelProvider`、`WorkspaceGateway`、`PluginRuntime`。这样可继续使用现有 SQLite/Reqwest，同时把桌面 UI、未来 CLI 或远程 API 放在同一用例层。

## 5. Rust/WASM 边界

Rust 必须拥有：状态机和依赖调度、权限/范围校验、凭据读取、provider 传输、SSE 解析、SQLite 事务、文件备份恢复、命令授权、事件序列和取消。WASM 只能是显式安装的纯数据变换插件：manifest + digest 验证后，以 JSON 输入调用固定 ABI；无文件、网络、时钟、环境变量、凭据和 host imports，且受 module size、memory、fuel、input/output 限制（`src/wasm.rs:88-220`）。

浏览器只应负责展示状态、收集用户意图和订阅事件；不能持有 Key、决定最终路径权限或直接执行命令。WASM 不应成为调度器、provider 客户端或任意脚本逃逸口；`run_wasm` 的错误、超时和输出必须被建模为一次工具调用结果。

## 6. 推荐实施阶段

1. **冻结契约**：从现有 endpoints、Task/Run JSON、事件类型和 WASM manifest 生成版本化 schema；补齐启动、配置、对话、Run、恢复的端到端验收矩阵。
2. **拆应用边界**：提取用例服务和端口，保留当前 HTTP 行为；将 `Engine` 中 provider、workspace、store、secret 的直接依赖改为接口，先不改变 UI。
3. **强化安全与生命周期**：明确本机认证、命令默认策略、取消超时、重试/幂等 TTL、任务归档/删除及数据库迁移；增加真实权限失败和异常退出测试。
4. **迁移产品能力**：在新模型上逐项决定旧会话、MCP/Skill、工作树和多项目是否迁移；只有确定契约后再加 provider capability 或远程协作。
5. **桌面化/发布**：将静态 UI、Rust 服务、数据目录、DPAPI 密钥和升级/备份流程打包；验证冷启动、端口占用、单实例、升级迁移和数据恢复。

## 7. 必须先询问的决策

1. 产品的支持边界是“单机项目代理”还是要兼容旧 Harness 的会话、MCP/Skill/插件生态？这决定迁移量和 API 是否要兼容。
2. 是否允许执行当前 Windows 用户权限的 PowerShell？若允许，是否改为显式审批、受限进程或项目级策略；写入范围本身无法约束命令。
3. 目标是否永远是 OpenAI Chat Completions 兼容服务，还是要支持 Responses/Anthropic/本地模型？这决定 `ModelProvider` 抽象和消息持久化格式。
4. 运行是否仅限单用户本机？若将来远程/多人使用，必须先定义认证、租户隔离、秘密托管和项目并发语义。
5. Run/Task 的数据保留、删除、归档和备份要求是什么？当前事件和消息会持续累积，且缺少过期清理。
6. WASM 插件是仅项目内预安装，还是需要安装、签名、版本升级和分发界面？这决定 manifest 信任根与插件仓库设计。
7. “恢复文件修改”应按任务全量回滚，还是按变更逐项审批？当前 API 以任务备份事件为粒度，无法表达更细的用户选择。

## 8. 可复核证据入口

- HTTP 契约：`src/server.rs:39-59`。
- 领域校验：`src/domain.rs:70-263`。
- 调度、依赖、限流、恢复：`src/engine.rs:137-537`。
- 持久化和事件：`src/store.rs:13-298`。
- 工作区权限和工具：`src/workspace.rs:31-332`。
- provider/SSE：`src/provider.rs:33-227`。
- WASM 边界：`src/wasm.rs:13-231`。
- 用户可见流程：`web/index.html`、`web/app.js`。
- 根目录旧系统入口与配置：`D:/peachsh-harness/README.md`、`package.json`、`start-peachsh.cmd`。
