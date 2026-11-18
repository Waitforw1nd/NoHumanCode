# NoHumanCode · Rust 核心工程 (`rust-app`)

本目录是 **NoHumanCode** 的核心 Rust Workspace（包名与二进制兼容标识保留为 `peachsh`，Edition 2024，SQLite `SCHEMA_VERSION = 18`）。

- **全局架构与 38 个源码模块职责索引**：见 [../../PROJECT-MAP.md](../../PROJECT-MAP.md)
- **项目开发准则**：见 [../../AGENTS.md](../../AGENTS.md)
- **长期产品设计理念**：见 [../../PROJECT-BOOK.md](../../PROJECT-BOOK.md)

## 工程结构

```text
rust-app/
├── Cargo.toml              # Workspace 根配置（主包 peachsh + 成员 crates/protocol）
├── build.ps1               # 自动探测 VS2022 MSVC / Windows SDK 并执行构建与门禁检查
├── crates/protocol/        # 零 I/O 依赖的共享协议类型（peachsh-protocol）
├── src/                    # CLI 工作台、Engine、SQLite Store、多模型流式与工具生态（38 个模块）
├── tests/                  # 13 个端到端与子系统集成测试目标 + support/ 夹具
└── web/                    # server.rs 本地 HTTP 服务内嵌兜底的轻量级静态 HTML/JS 资源
```

*(注：第一阶段聚焦后端 Host 与 CLI 交互式编码工作台；早期空壳 `crates/ui` 与 `src/wasm*` 已移除，图形界面延后至第二阶段规划。)*

## 构建与质量门禁（`build.ps1`）

`build.ps1` 会自动定位本机 Visual Studio C++ 工具链与 Windows 10/11 SDK，无需手动进入 Developer PowerShell：

```powershell
# 编译检查（全 Workspace 与测试目标）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action check

# 格式检查（rustfmt）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action fmt

# Clippy 静态分析（-D warnings 零警告门禁）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action clippy

# 运行全量单元与集成测试套件（cargo test --workspace --locked）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action test

# 构建 Release 优化二进制（target/release/peachsh.exe）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action build

# （可选）校验 web/ 兜底静态脚本
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action web-test

# （可选）需要真实上游计费密钥（PEACHSH_TEST_KEY）的连通冒烟测试
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action live
```

## 运行入口速览

### 1. 进程内 CLI AI 编程工作台（日常主力入口）

原生支持 **OpenAI Chat Completions**、**Anthropic Messages（含 Extended Thinking）** 与 **Google Gemini `streamGenerateContent`** 三大流式协议：

```powershell
# 方式 A：持久化初始化路由与 Windows DPAPI 加密凭据
.\target\release\peachsh.exe init --base-url https://api.openai.com/v1 --model gpt-4.1

# 方式 B：零初始化环境变量直连（支持 OPENAI_API_KEY / ANTHROPIC_API_KEY / GEMINI_API_KEY）
$env:OPENAI_API_KEY = "sk-..."
$env:NHC_MODEL = "gpt-4.1"

# 启动交互式流式编码 REPL（支持 @file:start-end 引用、行级彩色 Diff 预览与 [y/n/a] 审批）
.\target\release\peachsh.exe chat

# 续接最近会话或指定会话
.\target\release\peachsh.exe chat -c
.\target\release\peachsh.exe chat --resume <session_or_run_id>

# 单次非交互指令执行
.\target\release\peachsh.exe ask -y "阅读 @src/lib.rs 并概述模块划分"
```

REPL 常用斜杠命令：`/sessions`、`/resume <id>`、`/diff`、`/undo`、`/checkpoint`、`/restore <id>`、`/compact`、`/status`、`/model <name>`、`/auto`、`/new`。

### 2. 本地 HTTP / SSE Host 服务（脚本与客户端契约入口）

```powershell
# 启动本地 HTTP/SSE 服务（默认监听 127.0.0.1:3090）
.\target\release\peachsh.exe serve --port 3090 --data-dir .nhc-data --workspace .

# 执行启动与数据库迁移自检
.\target\release\peachsh.exe --data-dir .nhc-data --check
```

## 核心技术能力摘要

- **多协议原生流式**：`provider.rs` / `provider_transport.rs` / `provider_runtime.rs` 原生分流三大厂商流式推理、工具调用拼接与每轮 Token 用量统计（`provider_usage.rs`）。
- **编码工具与标准 MCP**：`workspace.rs` 提供 `.gitignore` 感知的 `glob_files` / `search_files`、支持批量与多 Hunk Unified Patch 的 `edit_file`、`read_file`、`write_file`、`git_status`、`todo_read/write`；`command_execution.rs` + `process_lifetime.rs` 基于 Windows JobObject 管控 PowerShell 子进程树；`mcp.rs` 支持 `.mcp.json` / `.nhc/mcp.json` 本地 JSON-RPC Stdio MCP 工具自动发现与调用。
- **可审阅与可恢复存储**：`approval.rs` + `git_diff.rs` 提供副作用前 Myers Unified Diff 审阅；`store.rs`（SQLite WAL `SCHEMA_VERSION = 18`）与 `repository.rs` 保障前镜像备份、Checkpoint 精确回滚、幂等回执重放以及进程中断后安全恢复为 `interrupted`。
