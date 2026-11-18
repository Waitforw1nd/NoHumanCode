# Sprint 01 工单与交付档案：基线修复、多厂商零初始化与 REPL 体验闭环

- **迭代周期**：Sprint 01（2026-11-18 启动）
- **总控 PM**：主会话项目经理
- **关联架构决策**：`ADR-001`、`ADR-002`、`ADR-003`、`ADR-004`（详见 [../ARCHITECTURE-DECISIONS.md](../ARCHITECTURE-DECISIONS.md)）
- **目标基线**：Baseline 0 (`108459ba`) -> Baseline 1（全量测试 100% 绿灯，文档承诺与 CLI 实装零断层）

---

## 工单列表与部门派单规格

### `S1-T0` [Tech-Runtime] 修复 Windows 8.3 短路径下 Worktree 创建误报 `worktree_corrupt`
- **背景痛点**：Windows 默认 `%TEMP%` 含 `ADMINI~1` 短文件名时，`tests/subsystems_e2e.rs::fast_worktree_creation_succeeds_even_with_untracked_files_in_source_repo` 因 `WorktreeReceipt.parent_path` 未规范化而与 `target_path` 校验不一致，触发 `WorktreeError::Corrupt`。
- **允许修改文件**：`src/worktree_management.rs`、`tests/subsystems_e2e.rs`
- **验收标准 (DoD)**：
  1. 按 `ADR-001` 统一 `receipt.parent_path` 的规范化展示路径并在 `WorktreeReceipt::validate()` 支持 8.3 短路径回退比对；
  2. 在默认 Windows `%TEMP%` 下实跑 `cargo test --test subsystems_e2e` 全部通过，且补充含短路径/未规范化父路径的单元测试。
- **技术组交付回执**：**已完成 (`Tech-Runtime`)**。在 `src/worktree_management.rs` 新增 `display_canonical_path` 去除 `\\?\` 前缀并统一由 `plan.target_parent.canonical_path()` 生成 `WorktreeReceipt.parent_path`；`WorktreeReceipt::validate()` 增加 `canonicalize` 短路径回退校验。新增单元测试 `validates_receipt_with_noncanonical_or_short_parent_path_and_strips_verbatim_prefix`，且 `subsystems_e2e` 在默认 Windows `%TEMP%` 下全绿通过。

---

### `S1-T1` [Tech-Runtime] 会话 Todo 跨进程持久化、Checkpoint 列表查询与动态路由模型续接
- **允许修改文件**：`src/workspace.rs`、`src/store.rs`、`src/engine.rs`、`tests/subsystems_e2e.rs`、`tests/git_diff.rs`
- **接口契约与规格**：
  1. **Todo 持久化恢复（`ADR-003`）**：
     - 在 `src/workspace.rs` 新增 `pub fn todos_for_task(task: &Task) -> Vec<TodoItem>`（先查 `RUN_TODOS` 缓存，未命中则逆序扫描 `task.messages` 中最新一次合法 `todo_write` 工具调用参数并回填缓存）；
     - `execute_builtin` 中 `"todo_read"` 改为调用 `todos_for_task(task)`，确保 `coda ask -c` 或进程重启后 Agent 调用 `todo_read` 仍能完整读回前序轮次的任务清单。
  2. **Checkpoint 列表查询（`ADR-003`）**：
     - 在 `Store` 与 `Engine` 新增 `pub fn checkpoints_for_tasks(&self, task_ids: &[String]) -> Result<Vec<crate::checkpoint::Checkpoint>>`，按创建时间/序号倒序返回匹配任务的全部已创建 Checkpoint。
  3. **多轮会话无缝同步最新路由模型（`ADR-002`）**：
     - 在 `Engine::send_chat_turn_locked`（`src/engine.rs`）构造下一轮 `Task` 时，若 `self.store.settings()` 中存在 `id == previous.spec.route_id` 的路由，采用最新配置的 `Route`（并执行 `self.key(&route)?` 校验），否则回退 `previous.route.clone()`。
- **验收标准 (DoD)**：不修改 `SCHEMA_VERSION = 18`，补充跨轮次/清除内存缓存后的 Todo 恢复测试、Checkpoint 列表查询测试与会话中途切换模型测试。
- **技术组交付回执**：**已完成 (`Tech-Runtime`)**。落地 `workspace::todos_for_task` + `clear_todo_cache_for_run`、`Store`/`Engine::checkpoints_for_tasks`（按 `rowid DESC` 返回），并在 `Engine::send_chat_turn_locked` 与 `store.rs` 续接快照校验中同步支持最新配置 `Route`。新增 `chat_turn_syncs_updated_route_model_recovers_todos_and_lists_checkpoints_newest_first` 与 `git_diff` 缓存清空恢复断言。

---

### `S1-T2` [Tech-Provider + Tech-Runtime] 三大协议实时流式 `reasoning_delta` 事件透出
- **允许修改文件**：`src/provider_runtime.rs`、`src/provider.rs`、`src/store.rs`、`src/repository.rs`、`src/engine.rs`、`tests/multi_provider_protocol.rs`
- **接口契约与规格（`ADR-004`）**：
  1. `src/provider_runtime.rs`：`CompletionDecoder` 新增带默认实现的 `fn feed_with_reasoning(&mut self, bytes: &[u8], emit: &mut Emit<'_>, emit_reasoning: &mut Emit<'_>) -> Result<(), CodecError>`（默认调用 `self.feed(bytes, emit)`，确保所有旧测试零改动通过），并在 `CompletionLease` 提供支持双回调的执行入口；
  2. `src/provider.rs`：OpenAI `Decoder`（`reasoning_content` / `reasoning`）、`AnthropicDecoder`（`thinking_delta`）、`GeminiDecoder`（`thought == true` 文本分片）在 `feed_with_reasoning` 中实时调用 `emit_reasoning(delta)`；正文 `emit` 与 `finish` 返回结构保持 100% 原有语义；
  3. `src/store.rs` & `src/engine.rs`：`Store` 新增 `pub(crate) fn reasoning_delta(&self, task_id: &str, safe_text: &str) -> Result<()>` 写入 `kind = "reasoning_delta"`、`data = {"text": safe_text}` 事件；`Engine` 为思考流配备独立的 `secrets::StreamRedactor`，实时脱敏写入事件表且**不追加进 `task.output`**。
- **验收标准 (DoD)**：既有 `provider_usage`、`http_contract`、`multi_provider_protocol` 全部保持兼容通过，并新增流式 `reasoning_delta` 事件落盘断言测试。
- **实现与验收进度**：三大协议已接入 `CompletionDecoder::feed_with_reasoning`。Engine 分别对正文和思考分片脱敏，Store 和 Repository 写入独立事件，`src/domain.rs` 已登记事件类型。解码回调和 CLI 输出分流测试通过。规格要求的落盘回读仍缺直接断言，需从 Store 读回事件并核对 `kind`、`data.text` 及对应任务的正文。

---

### `S1-T3` [Tech-CLI] 多厂商真零初始化、实时思考渲染与 REPL 全闭环增强
- **允许修改文件**：`src/cli.rs`、`tests/cli_workbench.rs`
- **接口契约与规格（`ADR-002` / `ADR-003` / `ADR-004`）**：
  1. **多厂商零初始化与 `--provider` 引导**：
     - `coda init` 新增 `--provider <openai|anthropic|gemini>` 可选参数，自动填充对应默认 Base URL、推荐模型与对应环境变量读取（`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY`）；
     - `ensure_route_ready` 完整支持三大厂商环境变量零初始化自动建路由（含根据 `--route` 或 `CODA_MODEL` 前缀智能匹配厂商）。
  2. **终端实时思考流展示**：
     - `execute_turn_and_stream` 消费 `"reasoning_delta"` 事件，在非 `--json` 模式下以暗灰样式（`\x1b[90m💭 ...\x1b[0m`）实时流式打印思考内容，切换到正文 `delta`、工具调用或审批提示前自动换行复位。
  3. **REPL 斜杠命令与多行输入补齐**：
     - 新增 `/checkpoints`：调用 `engine.checkpoints_for_tasks` 列出当前会话所有可用检查点（ID、任务、文件数、变更路径）；
     - 新增 `/routes`（及 `/route <id>`）：查看已配置路由列表并支持一键切换活跃路由；
     - 新增 `/todos`：调用 `coda::workspace::todos_for_task` 渲染当前会话的任务进度清单（`[✓] / [▸] / [ ]`）；
     - 升级 `/model <name>`：原地更新当前路由模型并**保留当前活跃会话 `session_state`** 无缝续聊；
     - 支持终端行尾 `\` 续行输入多行 Prompt。
- **验收标准 (DoD)**：新增 `ANTHROPIC_API_KEY` 与 `GEMINI_API_KEY` 零初始化集成测试、会话中途切换模型续聊测试及 REPL 辅助函数单元测试。
- **交付核验（2026-11-18）**：`cli_workbench` 6/6 通过。本地模拟服务验证了 Anthropic 和 Gemini 零初始化、思考与正文分流，以及 `ask -c --model` 在第二次请求携带首轮上下文。Anthropic 初始化 JSON 的默认地址断言已包含显式 HTTPS 端口 443。CLI 单元测试覆盖厂商选择和反斜杠续行。REPL 分支已核对，`/model` 保留活跃会话，`/route` 切至不同路由时开启新上下文。

---

### `S1-T4` [QA-Gate + PM] 全量门禁验收、导航文档同步与 Baseline 1 封板
- **允许修改文件**：`README.md`、`PROJECT-MAP.md`、`docs/PM-BOARD.md`、`docs/BASELINE-CHANGELOG.md`、`docs/sprints/SPRINT-01-zero-init-and-repl-ux.md`
- **验收标准 (DoD)**：
  1. `build.ps1 -Action fmt`、`clippy`、`test` 全量 100% 通过（0 失败、0 警告）；
  2. `README.md` 与 `PROJECT-MAP.md` 同步新增的斜杠命令（`/checkpoints`、`/routes`、`/todos`）、`coda init --provider` 与 `docs/` 导航说明；
  3. 更新 `BASELINE-CHANGELOG.md` 封板登记 Baseline 1。
- **门禁实测（2026-11-18）**：从仓库根运行 `build.ps1` 的 `fmt`、`check`、`clippy`、`test`，退出码均为 0。工作区共 132 项通过，0 项失败。另有 1 项需付费调用的 live 测试按默认配置跳过。Clippy 通过 `-D warnings`；部分测试夹具中的 Git 打印了 LF/CRLF 转换提示。原先失败的 Worktree 集成用例与路径规范化单元测试均通过。
- **基线状态**：README 和项目地图已同步。当前结果对应 `108459ba` 上的未提交工作树，待 S1-T2 的直接落盘断言补齐并重跑门禁后登记 Baseline 1。
