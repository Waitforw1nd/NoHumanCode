# Coda CLI

Windows 本机优先的旗舰级 AI 编程工作台：聚焦**多模型原生流式、可审阅、可回滚、可恢复、零仪式感**的 CLI 编码闭环。

## 目录结构与全局导航

- [PROJECT-MAP.md](PROJECT-MAP.md)：**现行项目地图（全局导航入口）**——架构拓扑、CLI、HTTP 与嵌入库的执行链、源码模块与集成测试矩阵
- [docs/PM-BOARD.md](docs/PM-BOARD.md)：**研发总控看板与基线台账**——迭代节奏、团队模块领地（[TEAM-CHARTER.md](docs/TEAM-CHARTER.md)）、架构决策录（[ARCHITECTURE-DECISIONS.md](docs/ARCHITECTURE-DECISIONS.md)）与已验证基线记录
- [AGENTS.md](AGENTS.md)：核心工程准则与日常构建/验证命令
- [PROJECT-BOOK.md](PROJECT-BOOK.md)：长期产品定位、设计理念与两阶段演进路线
- [src/](src) & [crates/protocol/](crates/protocol)：核心 Rust 工程（主包与二进制 `coda`，共享协议包 `coda-protocol`）
- [tests/](tests)：端到端与核心子系统集成测试矩阵
- `references/`：架构与同类产品参考资料

## 快速开始

### 1. 构建

在仓库根目录下直接执行：

```powershell
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action build
```

### 2. 模型路由与多厂商原生流式凭据

原生支持三大主流协议的流式推理与工具调用，无需额外部署中转层：

- **OpenAI Chat Completions**（及兼容接口）：`OPENAI_API_KEY`（可选 `OPENAI_BASE_URL`、`CODA_MODEL`）
- **Anthropic Messages API**（原生支持 Extended Thinking + Tool Use）：`ANTHROPIC_API_KEY`
- **Google Gemini `streamGenerateContent`**：`GEMINI_API_KEY`

独立 CLI 在 Windows 使用原有 DPAPI，在 macOS 使用登录 Keychain，在 Linux 和具备 D-Bus Secret Service 的 Android 终端使用系统钥匙串。SQLite 记录桌面钥匙串条目的引用。Windows、macOS 和 Linux 默认数据目录分别为 `%LOCALAPPDATA%\coda`、`~/Library/Application Support/coda`、`$XDG_DATA_HOME/coda`（缺省 `~/.local/share/coda`）。`CODA_DATA_DIR` 或 `--data-dir` 可指定数据目录。

```powershell
# 方式 A：一步式持久化初始化路由与加密凭据（支持 --provider openai|anthropic|gemini 快捷预设）
coda init --provider openai --model gpt-4.1
coda init --provider anthropic --route anthropic
coda init --provider gemini --route gemini

# 方式 B：零初始化（Zero-Init）环境变量直连开箱即用
$env:OPENAI_API_KEY = "sk-..."          # 或设置 ANTHROPIC_API_KEY / GEMINI_API_KEY
$env:OPENAI_BASE_URL = "https://api.openai.com/v1" # 可选（另有 ANTHROPIC_BASE_URL / GEMINI_BASE_URL）
$env:CODA_MODEL = "gpt-4.1"
```

### 3. 交互式工作台（`coda chat`）与单次执行（`coda ask`）

```powershell
# 启动交互式流式 REPL（支持行级彩色 Diff 预览与单键 [y/n/a] 审批）
coda chat

# 无缝续接当前目录最近一次会话，或指定 session_id / run_id 恢复多轮上下文
coda chat -c
coda chat --resume <session_or_run_id>

# 单次执行编码任务（同样支持 -y 自动审批、-c / --continue 跨调用续接与 @ 内联引用）
coda ask -y "解释 @src/lib.rs:1-38 并梳理 @src/ 目录职责"
coda ask -c "基于刚才的分析补充边界单元测试并运行验证"
```

#### 内联上下文引用与 REPL 斜杠命令

- **内联工作区引用**：在提问中直接写入 `@path/to/file`、`@path/to/file:start-end`（精确行范围）或 `@dir/`（目录清单），自动安全解析并注入带行号的 `<context_files>` 上下文。
- **交互式斜杠命令**（支持行尾 `\` 多行续行输入与思维链 `💭` 实时流式展示）：
  - `/sessions` & `/resume <id>`：列出当前项目近期会话并即时切换续接
  - `/diff` & `/undo`：查看工作区 Git 状态/变更统计与一键回滚最近一轮文件修改
  - `/checkpoint`、`/checkpoints` & `/restore <checkpoint_id>`：创建、列举并随时精确恢复持久化快照检查点
  - `/todos`：查看当前会话持久化的多步任务进度清单（跨 `coda ask -c` 与重启自动保持）
  - `/routes` & `/route <id>`：查看已配置模型路由列表并一键切换活跃路由
  - `/compact`：按原子工具调用组压缩长对话历史消息并展示节省体积
  - `/status` / `/model <name>` / `/auto` / `/new`：查看会话与 Token 用量统计、**保留当前对话上下文无缝切换模型**、切换自动审批或开启新会话

### 4. 启动本地 HTTP/SSE Host 服务（供脚本或远程客户端调用）

```powershell
coda serve --port 3090 --data-dir .coda-data --workspace .
```

## 核心编码工具与 MCP 生态

- **工程级内建工具集**：
  - `.gitignore` 感知的高速检索：`glob_files`（递归通配发现）与 `search_files`（正则/字面量及上下文行搜索）
  - 精准读写与补丁编辑：`read_file`（带行号分页读取）、`edit_file`（支持单处精确替换、`edits` 批量替换与多 Hunk Unified `patch` 模式，自动适配 CRLF/LF 并生成行级 Diff）、`write_file`（原子写盘）
  - 仓库状态、任务规划与受控终端：`git_status`（结构化工作区变更洞察）、`todo_write` / `todo_read`（多步工程任务清单追踪）、`run_command`（Windows JobObject 受控 PowerShell 子进程树回收）
- **标准 MCP（Model Context Protocol）stdio 生态**：
  - 自动发现工作区 `.mcp.json` 或 `.coda/mcp.json` 中配置的本地 MCP Server，通过标准 JSON-RPC 2.0 stdio 协议动态注册外部工具（`mcp__<server>__<tool>`），并统一纳入宿主机审批与安全审计闭环。
- **副作用可审阅、可回滚与长上下文韧性**：
  - 文件写入、外部命令与 MCP 调用均支持行级预览与细粒度审批；所有文件变更具备持久化前镜像备份（`/undo` 与 Checkpoint 恢复），自动加载项目 `AGENTS.md` / `CLAUDE.md` 规范并在长链路推理中保持上下文自动压缩与中断可恢复。

## 跨平台构建

使用 Rust stable（Edition 2024）和本机 C 工具链。Windows x64 运行上面的 MSVC 脚本，macOS arm64 需要 Xcode 命令行工具，Linux x64 需要 C 编译器和链接器。在仓库根执行：

```sh
cargo fmt --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
```

Windows 和 Linux 在准备好各自的凭据服务后，用 `cargo test --workspace --locked` 运行全量测试。macOS 从仓库根运行以下命令，使用隔离的临时系统钥匙串执行同一套测试和真实钥匙串用例：

```sh
swift -suppress-warnings tests/macos-keychain.swift
```

该入口向测试与 CLI 子进程传入 `CODA_KEYCHAIN_PATH`。手动指定时提供已解锁的现有钥匙串绝对路径，路径无效或钥匙串锁定会使凭据操作失败。

独立二进制位于 `target/release/coda`（Windows 为 `coda.exe`）。macOS/Linux 的独立 CLI 需要可解锁的登录钥匙串才能保存密钥或加密文件快照。SQLite 备份保存凭据引用，恢复后若对应钥匙串项已回收，需重新录入 API Key。Linux 的 Secret Service 使用 D-Bus；运行独立 CLI 全量测试时，建议在隔离的、已解锁的用户钥匙串会话中使用合成凭据。宿主纯内核测试在无桌面环境也可运行：

```sh
cargo test --locked --no-default-features --test embedded_kernel
```

Android arm64 交叉构建使用 Rust target `aarch64-linux-android` 和 NDK LLVM。以下示例使用 API 24 和 macOS 上 NDK 提供的 host 目录，`ANDROID_NDK_HOME` 指向已安装的 NDK 版本目录：

```sh
ndk_bin="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin"
export CC_aarch64_linux_android="$ndk_bin/aarch64-linux-android24-clang"
export AR_aarch64_linux_android="$ndk_bin/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$CC_aarch64_linux_android"
cargo build --locked --release --target aarch64-linux-android
cargo build --locked --release --no-default-features --lib --target aarch64-linux-android
```

Linux 编译主机将 `darwin-x86_64` 换成 NDK 的 `linux-x86_64`。Android 独立终端需提供可访问的 Secret Service 才能保存路由凭据。在 Tauri APK 中使用宿主提供的模型传输与 Android Keystore。接口、C3 v2 事件与运行示例见[接入说明](docs/EMBEDDING.md)。
