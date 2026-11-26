# 基线演进台账 (BASELINE-CHANGELOG)

> **定位**：记录每一个经过验证的项目基线锚点。每次合入主干前必须实测门禁并在此登记，确保任何时刻都具备清晰的回退锚点与事实依据。

---

## Baseline 7 · Sprint 05/S5-T2 独立子任务持久化与跨批次编排（2026-11-19，UTC+8）

- 验收版本为 `baseline-1`（`7e9f36d4`）加累计补丁 `S5-T2-from-baseline-1.patch`。Windows 本机与服务器核对的 SHA-256 均为 `0a545fbee6a9bb0f67337eae8ecbed2248cf8aaa9a65cbc391d8910d03b3fb73`。用户在新的独立 worktree 中成功执行了 `git apply --check` 和 `git apply`。
- 据用户回传的 Windows/MSVC 原生结果，`build.ps1` 的 `fmt`、`check`、`clippy`、`test` 退出码均为 0。默认套件 218 项通过、0 项失败、1 项忽略，唯一忽略项为付费在线测试 `coda_live_stream_and_coding_tool`。
- 本轮交付 SQLite v19 子任务持久化、按发送回执恢复与跨批次依赖编排。原生回归通过了跨会话依赖拒绝、撤权后阻止派发及并发续跑共享一次执行等用例。[Sprint 05 验收记录](sprints/SPRINT-05-parallel-exploration.md#验收记录)
- 本机验收 worktree 为 `E:\code\.tmp\coda-s5-t2-c1da887a2e`。完整 PowerShell 日志和测试明细分别位于 `%LOCALAPPDATA%\Temp\coda-s5-t2-c1da887a2e\verification.log` 和同目录的 `test.log`。

## Baseline 6 · Sprint 05/S5-T1 多 Agent 并行检索验收（2026-11-19，UTC+8）

- 验收版本为 `baseline-1`（`7e9f36d4`）加累计补丁 `S5-T1-from-baseline-1.patch`。服务器复核的 SHA-256 为 `e84305f644102312606db300e93f010478400bd195467670a80a526478ce2ae1`，用户确认 Windows 本机哈希一致。
- 用户回传的 Windows/MSVC 原生门禁中，`build.ps1` 的 `fmt`、`check`、`clippy`、`test` 退出码均为 0。本次回报未附原生测试用例计数。
- 本轮交付 `explore_parallel`。主会话可拆分 1–4 个只读检索分支，最多两支并发，按原顺序汇总摘要和行号证据。服务器回归另有 Wine 103 项通过，详见 [S5-T1 验收记录](sprints/SPRINT-05-parallel-exploration.md#服务器验证与-windows-本机验收)。

## Baseline 5 · Sprint 04/S4-T1 远程 MCP OAuth 验收（2026-11-19，UTC+8）

- 验收版本为 `baseline-1`（`7e9f36d4`）加累计补丁 `S4-T1-from-baseline-1.patch`。服务器复核的 SHA-256 为 `7c73a1f20ef4e8543195118ba6da2533a93fcd4b01a9c8aed04773a6e20b5a70`，用户确认 Windows 本机哈希一致。
- 用户回传 Windows/MSVC 原生门禁结果，`build.ps1` 的 `fmt`、`check`、`clippy`、`test` 退出码均为 0。本次回报未附原生测试用例计数。
- 本轮验收覆盖远程 MCP 的独立命令与聊天内登录、按工作区加密保存凭据和令牌刷新。服务器 Wine 与 Linux 的回归数据见 [S4-T1 验收记录](sprints/SPRINT-04-mcp-oauth.md#验收状态)。

## Baseline 4 · Sprint 03/S3-T3 多账号 KeyPool 验收（2026-11-19）

- 验收版本为 `baseline-1`（`7e9f36d4`）加累计补丁 `S3-T3-from-baseline-1.patch`。服务器复核的 SHA-256 为 `b32039f97d39222695b179f1537adaa7c0f27312690ff3db5cb5c9d8de959854`，用户确认 Windows 本机哈希一致。
- 用户回传的 Windows/MSVC 原生门禁中，`build.ps1` 的 `fmt`、`check`、`clippy`、`test` 退出码均为 0。本次回报未附原生测试用例计数。服务器的 Linux 与 Wine 测试明细见 [S3-T3 交付与验证](sprints/SPRINT-03-readonly-exploration.md#s3-t3-交付与验证)。

## Baseline 3 · Sprint 03/S3-T2 远程 MCP 验收（2026-11-19）

- 本机验证使用 `baseline-1`（`7e9f36d4`）加 `S3-T2-from-baseline-1.patch`。累计补丁 SHA-256 为 `12b7ad52b92a168a9871ccb1e37cbcb0bed5e9d7682eb2ef3bcb403e21770670`。Windows 端核对了哈希，并在独立 worktree 中成功执行 `git apply --check` 和 `git apply`。
- 用户回传的 Windows/MSVC 原生门禁中，`build.ps1` 的 `fmt`、`check`、`clippy`、`test` 均以 0 退出。170 项通过，0 项失败，1 项忽略。忽略项为默认跳过的付费在线测试 `coda_live_stream_and_coding_tool`。
- 本机验收 worktree 为 `E:\code\.tmp\coda-s3-t2-aa7784f9c1`。可见 PowerShell 输出和测试明细分别保存在 `%LOCALAPPDATA%\Temp\coda-s3-t2-aa7784f9c1\verification.log` 和同目录下的 `test.log`。[S3-T2 交付记录](sprints/SPRINT-03-readonly-exploration.md#s3-t2-服务器验证与原生门禁)

## Baseline 2 · Sprint 02 与 Sprint 03/S3-T1 联合验收（2026-11-19）

- Windows 本机从 `baseline-1`（`7e9f36d4`）创建独立 worktree，核对累计补丁 SHA-256 为 `4e78515fd92f7bf591cfc99204ae17c2cc50417c645420c4e1cc6473b0faba44`，补丁适用性检查与应用均通过。该提交加补丁是本次验收锚点。
- 用户回传的 Windows/MSVC 验证中，`fmt`、`check`、`clippy`、`test` 均以 0 退出。工作区 160 通过、0 失败、1 忽略，忽略项为付费在线测试 `coda_live_stream_and_coding_tool`。
- 覆盖 [Sprint 02](sprints/SPRINT-02-runtime-and-cli.md) 与 [S3-T1](sprints/SPRINT-03-readonly-exploration.md)。数据库与事件版本分别为 18 与 1。本次回报仅记录已应用补丁的验证工作树。

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
