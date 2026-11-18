# Coda CLI 研发组织章程与协作规范 (TEAM-CHARTER)

> **核心定位**：本文档定义 Coda CLI 项目的公司制研发分工（1 名项目经理 + 4 个专责技术组）、代码领地归属（Code Ownership）与基线门禁纪律。

---

## 1. 精益研发三条铁律（反形式主义与防回退）

1. **定额文档、拒绝文山会海**：研发管理文档严格收敛在 `docs/` 下的 4 份常驻台账（`PM-BOARD.md`、`TEAM-CHARTER.md`、`ARCHITECTURE-DECISIONS.md`、`BASELINE-CHANGELOG.md`）与每个迭代仅 1 份的 `docs/sprints/SPRINT-XX-*.md`。严禁创建零散的临时交接或流水账文件。
2. **权责隔离与领地归属**：每个技术组只允许修改工单授权范围内的模块文件；涉及跨组共享数据结构或函数签名变更，必须先在 `docs/ARCHITECTURE-DECISIONS.md` 中确立契约，再分头实施。
3. **硬门禁锁死基线**：任何迭代切片必须通过 `fmt`、`clippy -D warnings`、`test` 全绿验证，并核对 `README.md` / `PROJECT-MAP.md` 描述与代码实装 100% 一致后，方可登记入 `docs/BASELINE-CHANGELOG.md` 并提交主干。

---

## 2. 组织架构与代码领地归属表 (Code Ownership)

| 部门 / 角色 | 职责定位 | 管辖源码与测试领地（相对仓库根） |
| :--- | :--- | :--- |
| **PM / 总架构师** | 需求排期、跨模块契约拍板、工单派发、合并冲突仲裁、门禁终审与台账维护 | `docs/*`、`AGENTS.md`、`PROJECT-MAP.md`、`PROJECT-BOOK.md`、`README.md` |
| **Tech-CLI（CLI 与开发者体验组）** | `init / chat / ask` 开箱闭环、REPL 斜杠命令、终端流式/Diff 渲染、行输入体验与 CLI 模块解耦 | `src/cli.rs`（及 `src/cli/*`）、`src/credential_input.rs`、`src/main.rs`、`tests/cli_workbench.rs`、`tests/first_user_workflow.rs` |
| **Tech-Provider（模型路由与协议组）** | OpenAI / Anthropic / Gemini 原生流式协议、Thinking 增量透出、瞬态退避重试、多 Key 容灾与用量核算 | `src/provider.rs`、`src/provider_runtime.rs`、`src/provider_transport.rs`、`src/provider_usage.rs`、`src/budget.rs`、`src/network_authorization.rs`、`tests/multi_provider_protocol.rs`、`tests/provider_usage.rs` |
| **Tech-Runtime（核心引擎与工具组）** | `Engine` 调度、`Store` / `Repository` 存储、内建工具箱、Todo/Checkpoint 持久化、Worktree 管理、MCP 会话池与 Subagent | `src/engine.rs`、`src/store.rs`、`src/repository.rs`、`src/workspace.rs`、`src/mcp.rs`、`src/checkpoint.rs`、`src/git_worktree.rs`、`src/worktree_management.rs`、`src/tool_*.rs`、`crates/protocol/`、`tests/subsystems_e2e.rs`、`tests/runtime.rs`、`tests/git_diff.rs` |
| **QA-Gate（质量与基线门禁组）** | 全量回归验证、边界异常注入测试、Windows 环境兼容性检查、文档承诺与代码实现交叉核验 | `tests/*`、`build.ps1` |

---

## 3. 标准迭代运转 SOP（5 步闭环）

1. **立项派单（PM）**：在 `docs/PM-BOARD.md` 标记活跃工单，在 `docs/sprints/SPRINT-XX-*.md` 明确各工单的目标、文件边界、接口契约与验收标准（DoD）。
2. **专责研发（Tech Leads）**：各技术组在各自领地内完成编码与对应单元/集成测试，向 PM 提交结构化交付摘要（改动文件、核心逻辑、验证用例）。
3. **交叉审查（PM）**：审查跨模块集成点、安全脱敏边界（`secrets::seal/open/scrub`）及错误处理是否简洁实用。
4. **全量门禁（QA-Gate）**：在仓库根执行 `build.ps1 -Action fmt`、`clippy`、`test`，确保零警告、零失败，且无遗漏的文档断层。
5. **基线落盘（PM）**：回填 `SPRINT-XX-*.md` 交付回执，更新 `PROJECT-MAP.md`、`PM-BOARD.md` 与 `BASELINE-CHANGELOG.md`，执行原子 Git 提交。
