# Coda CLI 项目经理总控看板 (PM-BOARD)

> **跨窗口 30 秒恢复入口**：新会话接手时，请按顺序阅读：`AGENTS.md` -> `docs/PM-BOARD.md`（本文档） -> 当前活跃 `docs/sprints/SPRINT-XX-*.md` -> `PROJECT-MAP.md`。

---

## 1. 当前基线快照 (Current Baseline)

| 指标 | 状态 |
| :--- | :--- |
| **最新基线** | [Baseline 1](BASELINE-CHANGELOG.md)（本地 Git 标签 `baseline-1`） |
| **持久化版本** | SQLite WAL `coda.sqlite3` · `SCHEMA_VERSION = 18` · `EVENT_SCHEMA_VERSION = 1` |
| **当前阶段** | **Stage 1.5：CLI 编程工作台旗舰体验打磨与运行时加固**（Desktop GUI 暂缓至 Stage 2） |
| **最近完成的迭代** | [Sprint 01](sprints/SPRINT-01-zero-init-and-repl-ux.md) 全部工单已验收。Sprint 02 位于待办队列。 |
| **门禁健康度** | 2026-11-18 Windows/MSVC 的 `fmt`、`check`、`clippy -D warnings` 和工作区测试全部通过。133 项通过，0 项失败，1 项付费在线测试按默认配置跳过。 |

---

## 2. Sprint 01 实时作战看板

详细需求边界、接口契约与交付回执见：[docs/sprints/SPRINT-01-zero-init-and-repl-ux.md](sprints/SPRINT-01-zero-init-and-repl-ux.md)

| 工单 ID | 负责部门 | 任务摘要 | 状态 | 管辖核心文件 |
| :--- | :--- | :--- | :--- | :--- |
| **`S1-T0`** | `Tech-Runtime` | 修复 Windows 8.3 短路径（`%TEMP%` `ADMINI~1`）下 `WorktreeReceipt` 校验误报 `worktree_corrupt` | 🟢 Done | `src/worktree_management.rs`, `tests/subsystems_e2e.rs` |
| **`S1-T1`** | `Tech-Runtime` | 1) `todo_write/read` 基于 `Task.messages` 跨进程持久化恢复；2) `Store`/`Engine` 新增 Checkpoint 列表查询；3) `send_chat_turn` 动态同步最新路由模型 | 🟢 Done | `src/workspace.rs`, `src/store.rs`, `src/engine.rs` |
| **`S1-T2`** | `Tech-Provider` + `Tech-Runtime` | OpenAI、Anthropic 和 Gemini 均通过 SQLite 重开回读，验证思考事件、正文隔离及 OpenAI 夹具中的跨分片密钥脱敏 | 🟢 Done | `src/engine.rs`, `tests/multi_provider_protocol.rs` |
| **`S1-T3`** | `Tech-CLI` | 多厂商零初始化、实时思考输出、REPL 查询、模型切换和多行输入已落地；`cli_workbench` 6/6 通过 | 🟢 Done | `src/cli.rs`, `tests/cli_workbench.rs` |
| **`S1-T4`** | `QA-Gate` + `PM` | Windows 全量门禁通过，项目地图和 Baseline 1 台账已同步 | 🟢 Done | `PROJECT-MAP.md`, `docs/*` |

---

## 3. 中长期迭代路线图队列 (Product Backlog)

- **Sprint 02：运行时性能加固与 CLI 模块化重构**
  - `S2-T1`（`Tech-Runtime`）：MCP Stdio 会话级连接复用池与工具 Schema 缓存（消除每轮重复冷启动子进程，保留有状态 MCP 上下文）。
  - `S2-T2`（`Tech-Provider`）：上游 `429 / 502 / 503 / 504` 指数退避瞬态重试与 `Retry-After` 解析。
  - `S2-T3`（`Tech-CLI`）：将 5,000+ 行的 `src/cli.rs` 拆分为职责清晰的 `src/cli/` 子模块（`mod.rs`、`init_route.rs`、`repl.rs`、`mentions.rs`、`http_admin.rs`）。
- **Sprint 03：高阶 Agent 协同与生态扩展**
  - `S3-T1`（`Tech-Runtime` + `Tech-CLI`）：基于 `Engine` 并发底座向主编码会话暴露轻量只读探索子智能体工具（Subagent Exploration），保护主上下文窗口。
  - `S3-T2`（`Tech-Provider` / `Tech-Runtime`）：多账号 KeyPool 故障转移 或 Streamable HTTP 远程 MCP 支持。
- **Stage 2 储备：原生桌面审阅工作台 (Desktop GUI)**
  - 待 Stage 1.5 CLI 核心与协议事件全部稳固后，基于 `coda-protocol` 与 `coda serve` 启动多文件 Diff 审阅界面。
