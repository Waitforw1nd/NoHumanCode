# 基线演进台账 (BASELINE-CHANGELOG)

> **定位**：记录每一个经过验证的项目基线锚点。每次合入主干前必须实测门禁并在此登记，确保任何时刻都具备清晰的回退锚点与事实依据。

---

## M3 前置 B 分支验收（2026-11-25）

- 基于 `fd7d4b9b`，分支 `feat/embedded-kernel-platforms`。Coda 宿主使用 `default-features = false` 和进程内 `EmbeddedEngine`；独立 CLI 使用默认特性。契约见[接入说明](EMBEDDING.md)。
- macOS arm64、Linux x64 和 Windows x64 均通过全目标检查、工作区测试和 release 构建。macOS 实测 `--help`、`init`、重启后 `/sessions`。macOS Keychain 和 Linux GNOME Keyring 均实测复制库轮换、回滚、重开和删除，Windows 上用 `fd7d4b9b` 的 CLI 写入合成 DPAPI 凭据，新版 CLI 在新进程内用同一凭据通过本机模拟模型鉴权。
- Android arm64 使用 NDK 28 和 API 24 交叉链接了 release CLI、纯内核库与嵌入测试程序。设备运行、Coda 产品接线与真实模型端到端回合待验。
- 21 项公开嵌入契约测试通过。LLVM 对 `embedded/mod.rs`、`embedded/turn.rs`、`model_loop.rs` 的合计行覆盖率为 96.65%。付费 live 用例保持手动执行。

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
