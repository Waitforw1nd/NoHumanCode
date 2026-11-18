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
