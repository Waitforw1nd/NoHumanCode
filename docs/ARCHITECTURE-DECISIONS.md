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
  2. `Engine` 经 `StreamRedactor` 脱敏后写入 `events` 表独立事件类型 `kind = "reasoning_delta"`（载荷 `{"text": "..."}`），不计入正文 `task.output`（仅当模型整轮无正文且无工具调用时保留原有回退行为）；
  3. CLI 工作台捕获 `reasoning_delta` 事件，在终端以轻量暗色样式实时流式渲染思考过程，进入正文或工具调用时自动换行收束。
- **影响范围**：`src/provider_runtime.rs`、`src/provider.rs`、`src/store.rs`、`src/engine.rs`、`src/cli.rs`。

## ADR-005　进程内嵌入边界（M3 前置 B，2026-11-25）

`EmbeddedEngine::new` 接受宿主提供的 `ModelTransport`、`ToolHost` 和 `SessionStore`。构造仅保存依赖。独立 CLI 和本地 HTTP 服务继续通过 `Engine::new` 装配 SQLite、原生模型协议、内建工具和审批。两种构造入口允许同一进程分别管理宿主身份与独立 CLI 状态。

宿主每轮交付模型选择、提示词和当前分支历史，每次模型请求前提供当时的工具声明。内核在首次追加用户消息前预检完整请求。宿主存储按修订号原子追加，内核先保存整批工具意图，再按序审查、执行并保存结果。工具通过 `is_error` 报告的失败随同批结果交给模型，以便模型据此修正操作。审查拒绝或宿主审查、执行接口返回 `Err` 时，内核停止派发并记录剩余调用已跳过。取消令牌属于特定回合，显式中止等到宿主清理完成；观察者断开后执行仍可继续。压缩通过相同的会话独占与存储接口追加摘要，宿主保留原始历史与分支。

**回合内输入。** `run_with_input` 将输入句柄绑定到一次运行，使同会话后续回合无法消费旧队列。工作线程在每次生成请求前选取全部插话，先用最新工具集预检，再以会话修订号原子提交并发出交付事件。这样一对工具意图与结果之间不会插入用户消息。终止时宿主从原句柄取回尚未提交的用户和自定义消息；已提交的自定义消息完整入档，模型只接收正文。宿主持有工具注册表，并在执行前审查每项调用。公开接口和取消边界见[回合中插话与取回](EMBEDDING.md#回合中插话与取回)。

**请求前压缩。** 宿主在 `ModelTransport::before_request` 按实际生成请求及提供商策略决定阈值、指令和 `tokens_before`。内核沿用当前会话的独占权和选定修订，共用显式压缩的无工具摘要及原子追加。每个生成边界最多压缩一次，再以宿主重载的投影和未提交输入执行预检。这样工具结果、插话交付和取消沿用同一个写入顺序。详见[回合内请求前压缩](EMBEDDING.md#回合内请求前压缩)。

## ADR-006　平台与凭据边界（2026-11-25）

Windows x64 继续读取同一用户原有的 DPAPI 密文。macOS arm64 的独立 CLI 直接将 API Key 存入系统 Keychain，Linux x64 及可访问 Secret Service 的 Android arm64 终端将其存入用户钥匙串。数据库保存版本化随机引用；轮换和回滚与配置事务协调。较大的工作区快照使用钥匙串保护的独立密钥加密。嵌入构建关闭 `standalone`，密钥、身份、SSO 与 SQLite 均由宿主自行选择。

Windows Job Object、PowerShell 和控制台处理按 Windows 条件编译。Unix 命令经系统 shell 执行，采用独立进程组管理取消与超时。编译目标、工具链和 Android 运行条件见[构建说明](../README.md#跨平台构建)。
