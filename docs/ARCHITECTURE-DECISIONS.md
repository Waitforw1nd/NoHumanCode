# 架构决策记录 (ARCHITECTURE-DECISIONS)

> **定位**：用精炼卡片记录关键技术取舍、跨组接口契约与不变量约束，防止跨会话协作时误破既有设计边界。

---

## ADR-001：Windows 8.3 短路径下 Worktree 回执的路径一致性归一化
- **日期 / 状态**：2026-11-18 · 已采纳（Sprint 1 `S1-T0`）
- **背景**：Windows 默认 `%TEMP%` 常含 8.3 短文件名（如 `C:\Users\ADMINI~1\...`），而 `PersistedWorktreePlan` 使用 `canonicalize` 解析为长路径（`\\?\C:\Users\Administrator\...`）。原 `claim_worktree` 将未规范化的 `request.parent_path` 与规范化后的 `target_path` 同时写入 `WorktreeReceipt`，导致 `WorktreeReceipt::validate()` 按字面路径键比对失败并误报 `WorktreeError::Corrupt`。
- **决策**：
  1. `claim_worktree` 生成 `WorktreeReceipt` 时，`parent_path` 统一采用 `plan.target_parent.canonical_path()` 去除 `\\?\` verbatim 前缀后的规范绝对路径，确保与 `target_path` 同源；
  2. `WorktreeReceipt::validate()` 在直接 `windows_path_key` 比对不等时，对存在的 `parent_path` 回退尝试一次 `canonicalize` 归一化比对，兼顾历史持久化回执兼容性。
- **影响范围**：`src/worktree_management.rs`、`tests/subsystems_e2e.rs`。

---

## ADR-002：多厂商零初始化环境变量自动路由与无缝切模型契约
- **日期 / 状态**：2026-11-18 · 已采纳（Sprint 1 `S1-T1` & `S1-T3`）
- **背景**：`README.md` 与 `PROJECT-MAP.md` 承诺配置 `OPENAI_API_KEY`、`ANTHROPIC_API_KEY` 或 `GEMINI_API_KEY` 即可零初始化开箱使用，但 `src/cli.rs` 此前仅识别 `OPENAI_API_KEY`；且 `Engine::send_chat_turn_locked` 直接克隆上一轮 `previous.route`，导致 REPL `/model <name>` 被迫清空会话。
- **决策**：
  1. **多厂商预设表**：
     - `openai`：`OPENAI_API_KEY` | 默认 Base URL `OPENAI_BASE_URL` 或 `https://api.openai.com/v1` | 默认模型 `gpt-4.1`
     - `anthropic`：`ANTHROPIC_API_KEY` | 默认 Base URL `ANTHROPIC_BASE_URL` 或 `https://api.anthropic.com/v1` | 默认模型 `claude-sonnet-4-20250514`
     - `gemini`：`GEMINI_API_KEY` | 默认 Base URL `GEMINI_BASE_URL` 或 `https://generativelanguage.googleapis.com/v1beta` | 默认模型 `gemini-2.5-pro`
  2. **零初始化推断优先级**：若用户显式指定 `--route anthropic|gemini|openai` 则优先匹配对应厂商环境变量；否则若 `CODA_MODEL` 前缀匹配对应厂商（`claude-*` / `gemini-*`）且对应 Key 存在则优先选用；其余情况按 `OPENAI_API_KEY` -> `ANTHROPIC_API_KEY` -> `GEMINI_API_KEY` 顺序自动建路由并经 DPAPI 落盘。`coda init` 同步新增 `--provider <openai|anthropic|gemini>`。
  3. **会话内无缝切模型**：`Engine::send_chat_turn_locked` 在续接下一轮时，优先从最新 `settings.routes` 按 `previous.spec.route_id` 读取最新 `Route`（含更新后的 `model` / `max_tokens`），使 REPL `/model <name>` 与 `coda ask -c --model <name>` 原地保留上下文续聊。
- **影响范围**：`src/engine.rs`、`src/cli.rs`、`tests/cli_workbench.rs`。

---

## ADR-003：零 Schema 破坏的会话 Todo 持久化与 Checkpoint 列表查询
- **日期 / 状态**：2026-11-18 · 已采纳（Sprint 1 `S1-T1`）
- **背景**：`todo_write` 此前仅写入内存静态表 `RUN_TODOS`，跨 `coda ask -c` 进程退出即丢失；`checkpoints` 表虽已落盘，但缺少列表查询接口供 REPL `/checkpoints` 使用。
- **决策**：
  1. **保持 `SCHEMA_VERSION = 18` 不变**：由于每次 `todo_write` 工具调用及其参数均已原子持久化在 `Task.messages` 中并随 `send_chat_turn` 跨轮传递，`workspace::todos_for_task(task)` 在内存 `RUN_TODOS` 未命中时自动逆序扫描 `task.messages` 中最近一次合法 `todo_write` 调用回填缓存，实现零迁移成本的跨进程/重启持久化。
  2. **Checkpoint 列表接口**：在 `Store` 与 `Engine` 上暴露按任务 ID 集合（或会话关联任务链）查询 `Vec<Checkpoint>` 的只读方法（按创建时间/序号倒序），供 CLI `/checkpoints` 直接展示。
- **影响范围**：`src/workspace.rs`、`src/store.rs`、`src/engine.rs`、`src/cli.rs`。

---

## ADR-004：流式思维链（Reasoning / Thinking）独立事件通道透出
- **日期 / 状态**：2026-11-18 · 已采纳（Sprint 1 `S1-T2` & `S1-T3`）
- **背景**：OpenAI `reasoning_content`/`reasoning`、Anthropic `thinking_delta`、Gemini `thought: true` 在长思考期间无实时流式反馈，但又绝不能混入正文 `delta` 事件或污染 `task.output`（否则会破坏纯正文契约与既有测试）。
- **决策**：
  1. 在 `CompletionDecoder` 上增加向后兼容的 `feed_with_reasoning(&mut self, bytes, emit_text, emit_reasoning)`（默认转发至 `feed`，保持既有调用方零改动兼容），三大协议解码器在收到思考增量时实时触发 `emit_reasoning`；
  2. `Engine` 在思考事件入口保留可能跨分片的密钥前缀，经 `secrets::scrub` 对当前路由密钥精确脱敏后写入 `events` 表独立事件类型 `kind = "reasoning_delta"`（载荷 `{"text": "..."}`），不计入正文 `task.output`（仅当模型整轮无正文且无工具调用时保留原有回退行为）；
  3. CLI 工作台捕获 `reasoning_delta` 事件，在终端以轻量暗色样式实时流式渲染思考过程，进入正文或工具调用时自动换行收束。
- **影响范围**：`src/provider_runtime.rs`、`src/provider.rs`、`src/store.rs`、`src/engine.rs`、`src/cli.rs`。

## ADR-005：MCP 会话归属与重试边界（Sprint 02）

- 一个 Engine 内，同一会话、工作区和服务配置可共享 MCP stdio 进程及工具定义。发现与调用使用相同的连接，调用前沿用现有权限、审批与配置检查。配置变更、进程失败或 Engine 关闭时释放相应资源。工具已派发时，取消、超时或断链均沿用现有审批的未知结果处理。任务保留取消状态和未知结果提示，恢复时返回对应的审批 ID。
- 上游仅在收到明确的 429、502、503、504 响应时有限重试，遵守 Retry-After 和等待上限。每次发送都须保持原有网络授权、审计和预算语义。成功响应的流一旦开始，不再重发。
- CLI 模块拆分沿用现有可执行入口、参数、输出与持久化协议。

## ADR-006 · 只读代码探索（Sprint 03）

- 主编码任务可调用 `explore_files`，传入 `question` 和 `paths`。每次选取一至四个明确的工作区相对文件，沿用文件工具的权限、路径和内容边界。模型可先用已有文件检索工具定位路径。
- 子请求有独立上下文，包含问题和带行号的有限文件摘录，使用父任务的路由、授权及预算。子请求不给模型分配工具，父任务只收到有长度上限的摘要。取消、审计及用量结算沿用现有模型请求机制。
- CLI 使用既有工具事件显示探索进度。此次切片沿用数据库与 HTTP 协议版本。远程 MCP 与 KeyPool 等后续工单独立排期。

## ADR-007 · MCP Streamable HTTP 工具客户端（Sprint 03/S3-T2）

- 采用 [MCP 2025-06-18 Streamable HTTP](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)。发现与调用沿用同一会话池，客户端支持 JSON 与 SSE 响应、协议版本协商及服务端分配的会话 ID。已派发的调用沿用未知结果与禁止自动重放的处理。
- Host 通过环境变量 `CODA_MCP_HTTP_SERVERS` 接受按工作区登记的 JSON 数组。每项含绝对 `workspace`、`name`、`url`，可选 `bearerTokenEnv` 指向 Host 端凭据变量。HTTP 仅限明确回环地址，其他地址使用 HTTPS。凭据与会话 ID 绑定到登记端点，禁用重定向。已有工作区 stdio 配置继续生效。
- 沿用项目工具、命令权限与逐次审批。Host 登记的服务名在相应工作区拥有独立归属，工作台在审批时展示已登记的目标。支持工具发现、调用与会话关闭，OAuth 注册与授权流程单独排期。

## ADR-008 · 多账号 KeyPool（Sprint 03/S3-T3）

- Host 通过环境变量 `CODA_KEY_POOLS` 登记 JSON 数组。每项含 `routeId`、`baseUrl` 和备用密钥环境变量名数组 `keyEnvs`。路由 ID 与完整规范化服务地址均须匹配。主凭据沿用原路由，备用成员由启动进程显式提供。配置示例只列变量名。
- 同一 Engine 的模型请求在可用成员之间轮询。单次操作最多发送三次。对明确的 HTTP 429 记入成员冷却并遵守 Retry-After；取消、授权或预算失败、响应开始后的中断都终止当前操作。每次实际发送仍以逻辑路由和项目重新授权、预留预算并记录回执。账号余额查询沿用账号专用凭据。
- 工作区配置不能指定备用凭据。Host 向子进程传递环境时排除已登记的密钥变量。测试使用合成密钥和回环服务，覆盖实际请求头、轮询、失败切换、预算及凭据保护。

## ADR-009 · 远程 MCP OAuth 登录与刷新（S4-T1）

- 在 Host 的 `CODA_MCP_HTTP_SERVERS` 登记中，`oauth` 配置含预注册公共客户端 `clientId`、授权服务器 `issuer` 和可选 `scopes`。显式 OAuth 与静态 Bearer 互斥。凭据绑定到规范工作区、服务名、资源 URL 和上述配置；用户按工作区显式执行 `coda mcp login <server>`。
- 登录采用浏览器授权码、S256 PKCE、一次性 state 和本机回环回调。通过受保护资源及授权服务器元数据核对登记信息，授权、换码和刷新均绑定资源 URL。授权服务器的端点使用 HTTPS，显式回环测试服务可使用 HTTP。[MCP 2025-06-18 授权规范](https://modelcontextprotocol.io/specification/2025-06-18/basic/authorization)
- 复用现有 Windows DPAPI 凭据存储。刷新令牌轮换时一起更新，交互授权仅由用户显式发起。工具调用沿用既有审批；派发后不因鉴权失败自动重放。

## ADR-010 · 单轮对话中的并行只读检索（S5-T1）

- 主模型调用 `explore_parallel`，输入 `branches` 为 1–4 个包含 `question`、`path`、`query` 的对象。Host 校验整批目录，在允许读取的工作区中按字面检索并收集带行号的上下文。每支最多取四个文件。无命中直接返回，各子模型仅看到本支证据。
- 每批最多两支同时发送，第一支使用父任务全局名额，第二支仅使用立即可用的额外名额。模型发送沿用父任务的网络授权、预算、凭据并发及用量回执。父任务独占写入消息和用量，停止派发后为在途请求结算。
- 父模型收到按输入排序的 `branches`，每支有从 1 开始的 `index`、`status`（`ok`、`no_matches` 或 `error`）和 `evidence`（含相对 `path`、`start_line`、`end_line` 的数组），成功时附 `summary`，失败时附脱敏的 `error`。进度走标准错误。中断的工具组恢复时沿用可能产生用量的提示。

## ADR-011 · 持久化的只读探索子任务（S5-T2）

- SQLite v19 保存批次、稳定子任务 ID、输入序号、结果、依赖及尝试与网络回执的关联。批次归属父任务所在的项目、工作区、会话。相同父任务和调用标识重放须核对输入；同批依赖与越界引用在提交事务内拒绝。
- 每支结果独立写入。网络准入同时登记子任务尝试，恢复时对照回执处理未发送、已完成与可能已计费的分支。显式重试开启新尝试。同步工具保留原结果，并返回批次及子任务 ID。独立提交可先取得标识，再由当前运行的 CLI 调度就绪任务。
- 依赖只引用较早批次的子任务 ID，字段为每支可选的 `depends_on`。只有成功或无命中结果能供后续分支读取；等待依赖期间不占模型并发名额。旧版未登记批次的历史工具调用沿用原有恢复行为。
