# 基线演进台账 (BASELINE-CHANGELOG)

> **定位**：记录每一个经过验证的项目基线锚点。每次合入主干前必须实测门禁并在此登记，确保任何时刻都具备清晰的回退锚点与事实依据。

---

## Baseline 1 · Sprint 01 验收（2026-11-18）

- 本地 Git 标签为 `baseline-1`，前一提交为 `fd7d4b9b`。数据库 schema 版本为 18，事件 schema 版本为 1。
- 本轮收尾验证了 OpenAI、Anthropic 和 Gemini 流式事件。测试关闭 Engine，重开 SQLite 数据库，分别核对思考事件、正文事件与任务输出。OpenAI 夹具还验证跨分片路由密钥脱敏和普通文本保留。
- Windows/MSVC 四项门禁均以 0 退出，分别为 `build.ps1 -Action fmt`、`check`、`clippy`（`-D warnings`）和 `test`。工作区共 133 项通过、0 项失败，1 项需凭据和付费调用的在线测试默认跳过。
- Sprint 01 的五项工单均已完成。详见[迭代验收记录](sprints/SPRINT-01-zero-init-and-repl-ux.md)。

## [Baseline 0 — 初始盘点基线] `108459ba` (2026-11-18)

- **基线提交**：`108459ba` (`refactor!: rename project to Coda CLI (coda) and promote workspace to root`)
- **Schema 版本**：`SCHEMA_VERSION = 18`（SQLite WAL `coda.sqlite3`）
- **代码规模**：`src/` + `crates/protocol/` 共 39 个 Rust 源文件（33,788 行），`tests/` 共 17 个文件（8,408 行）
- **PM 初始门禁实测结果**：
  - `build.ps1 -Action fmt`：通过
  - `build.ps1 -Action clippy`（`-D warnings`）：通过（零警告）
  - `build.ps1 -Action test`：单元与集成测试共 **116 通过 / 1 失败**
    - **已定位基线缺陷**：`tests/subsystems_e2e.rs::fast_worktree_creation_succeeds_even_with_untracked_files_in_source_repo` 在 Windows 默认 `%TEMP%` 含 8.3 短路径（`ADMINI~1`）时触发 `worktree_corrupt`（已立项 `S1-T0` 优先修复，见 `ADR-001`）。
    - **已定位文档断层**：`README.md` 与 `PROJECT-MAP.md` 承诺的 `ANTHROPIC_API_KEY` / `GEMINI_API_KEY` 零初始化直连尚未在 `src/cli.rs` 实装（已立项 `S1-T3` 修复，见 `ADR-002`）。
