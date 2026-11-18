# Coda CLI 项目经理总控看板 (PM-BOARD)

> **跨窗口 30 秒恢复入口**：新会话接手时，请按顺序阅读：`AGENTS.md` -> `docs/PM-BOARD.md`（本文档） -> 当前活跃 `docs/sprints/SPRINT-XX-*.md` -> `PROJECT-MAP.md`。

---

## 1. 当前基线快照 (Current Baseline)

| 指标 | 状态 |
| :--- | :--- |
| **最新锚点 Commit** | `108459ba`（Baseline 0，正在推进 Sprint 1 升级至 Baseline 1） |
| **持久化版本** | SQLite WAL `coda.sqlite3` · `SCHEMA_VERSION = 18` · `EVENT_SCHEMA_VERSION = 1` |
| **当前阶段** | **Stage 1.5：CLI 编程工作台旗舰体验打磨与运行时加固**（Desktop GUI 暂缓至 Stage 2） |
| **当前活跃迭代** | **[Sprint 01：基线修复、多厂商真零初始化与 REPL 全闭环体验](sprints/SPRINT-01-zero-init-and-repl-ux.md)** |
| **门禁健康度** | 2026-11-18 在当前工作树通过 `fmt`、`check`、`clippy -D warnings` 和工作区测试（132 通过，0 失败，1 项付费 live 测试默认跳过）。Windows Worktree 回归通过。 |

---

## 2. Sprint 01 实时作战看板

详细需求边界、接口契约与交付回执见：[docs/sprints/SPRINT-01-zero-init-and-repl-ux.md](sprints/SPRINT-01-zero-init-and-repl-ux.md)

| 工单 ID | 负责部门 | 任务摘要 | 状态 | 管辖核心文件 |
| :--- | :--- | :--- | :--- | :--- |
| **`S1-T0`** | `Tech-Runtime` | 修复 Windows 8.3 短路径（`%TEMP%` `ADMINI~1`）下 `WorktreeReceipt` 校验误报 `worktree_corrupt` | 🟢 Done | `src/worktree_management.rs`, `tests/subsystems_e2e.rs` |
| **`S1-T1`** | `Tech-Runtime` | 1) `todo_write/read` 基于 `Task.messages` 跨进程持久化恢复；2) `Store`/`Engine` 新增 Checkpoint 列表查询；3) `send_chat_turn` 动态同步最新路由模型 | 🟢 Done | `src/workspace.rs`, `src/store.rs`, `src/engine.rs` |
| **`S1-T2`** | `Tech-Provider` + `Tech-Runtime` | 三大协议解码器、独立脱敏与事件写入已实现，待补持久化事件回读断言 | 🟡 待补验收用例 | `src/provider*.rs`, `src/domain.rs`, `src/repository.rs`, `src/store.rs`, `src/engine.rs`, `tests/multi_provider_protocol.rs` |
| **`S1-T3`** | `Tech-CLI` | 多厂商零初始化、实时思考输出、REPL 查询、模型切换和多行输入已落地；`cli_workbench` 6/6 通过 | 🟢 Done | `src/cli.rs`, `tests/cli_workbench.rs` |
| **`S1-T4`** | `QA-Gate` + `PM` | 工作树门禁已通过，导航已同步。补齐 S1-T2 验收用例并复跑后登记 Baseline 1 | 🟡 验收中 | `tests/*`, `docs/*`, `README.md`, `PROJECT-MAP.md` |

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
