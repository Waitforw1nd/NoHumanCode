# Coda CLI

Windows 本机优先的旗舰级 AI 编程工作台：聚焦**多模型原生流式、可审阅、可回滚、可恢复、零仪式感**的 CLI 编码闭环。

## 目录结构与全局导航

- [PROJECT-MAP.md](PROJECT-MAP.md)：**现行项目地图（全局导航入口）**——架构拓扑、两条核心执行主链、Rust 源码模块职责与集成测试矩阵
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

### 2. 模型路由与多厂商原生流式凭据（支持 Windows DPAPI 加密存储或零初始化环境变量）

原生支持三大主流协议的流式推理与工具调用，无需额外部署中转层：

- **OpenAI Chat Completions**（及兼容接口）：`OPENAI_API_KEY`（可选 `OPENAI_BASE_URL`、`CODA_MODEL`）
- **Anthropic Messages API**（原生支持 Extended Thinking + Tool Use）：`ANTHROPIC_API_KEY`
- **Google Gemini `streamGenerateContent`**：`GEMINI_API_KEY`

默认本机数据目录为 `%LOCALAPPDATA%\coda`（数据库文件 `coda.sqlite3`，可通过环境变量 `CODA_DATA_DIR` 或 `--data-dir` 覆盖）。

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

#### 为同一路由配置多个账号

在启动 `coda chat`、`coda ask` 或 `coda serve` 的 PowerShell 中设置 `CODA_KEY_POOLS`。下例沿用路由 `default` 的主密钥，并从两个已由 Host 环境提供的变量读取备用密钥。JSON 中只填写变量名。路由 ID 和规范化后的完整服务地址（含协议、端口和路径）必须同时匹配。缺失的备用变量会跳过。

```powershell
$env:CODA_KEY_POOLS = '[{"routeId":"default","baseUrl":"https://api.openai.com/v1","keyEnvs":["BACKUP_A","BACKUP_B"]}]'
coda chat
```

在同一运行中的 Engine 内，任务推理、只读探索、模型发现和路由检查轮流使用可用凭据。成员收到 HTTP 429 时进入冷却，冷却时间采用上游的 `Retry-After`。还有可用成员时可在本次请求内切换；全部成员冷却时结束本次调用。HTTP 502、503、504 使用退避重试。在同一凭据上重试若需要等待超过 10 秒，本次调用直接返回错误。每个模型列表或推理请求最多发送三次，每次发送都重新检查授权并记录回执，推理请求还会检查预算。多成员回执中的 `credential_slot` 从 0 起，0 为主凭据，后续编号对应本次有效且去重的备用凭据。取消、授权或预算失败、成功响应断流都终止调用。账号余额查询使用账号自己的凭据。

轮询位置和冷却时间保存在当前进程中。`coda chat` 和 `coda serve` 可以跨轮复用；单独启动一次 `coda ask` 会重新开始轮询。命令和本地 stdio MCP 子进程的环境会过滤已登记的密钥变量。

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

在 `coda ask` 或 `coda chat` 中，可以让主会话把独立的代码问题交给 `explore_parallel`。在本仓库中可以这样提问。

```powershell
coda ask "请用 explore_parallel 在 src/mcp 中检索 invalid_grant，在 tests 中检索 reject_discovery，说明凭据失效时 CLI 怎么处理，并引用路径和行号。"
```

每次可给出 1–4 个分支，每支指定一个问题、工作区相对目录和区分大小写的字面检索词。每支取最多 4 个文件的命中行及相邻上下文，子模型使用父任务的路由和项目预算。没有命中时直接返回空结果。最多两支并发，只有一个可用名额时顺序执行。主会话按问题顺序收到摘要和证据位置，终端进度写入标准错误。[实现与验收](docs/sprints/SPRINT-05-parallel-exploration.md)

### 继续上次的探索，或让下一批使用已保存的证据

在 `coda chat` 输入 `/jobs` 可列出当前会话的子任务。`/jobs show <子任务ID>` 可以查看状态、摘要、行号与发送回执。重开同一数据目录和工作区后，`/resume <会话ID>` 还可以找回这些记录。已完成和无命中的分支直接复用。排队分支的 `blocked_by` 会列出仍在等待的前置 ID 及状态。如果状态为 `unknown`，请求可能已产生用量，可查看回执后用 `/jobs retry <子任务ID>` 显式发起新尝试。

`coda jobs continue`、`coda jobs retry` 和聊天内对应的 `/jobs` 命令在执行中支持 Ctrl+C。中断会停止后续派发，等待当前尝试写回状态；聊天内会返回提示符。用 `jobs show` 或 `/jobs show` 核对回执：普通 `continue` 不会重发结果未知的请求，也不会重跑已完成的分支。

脚本也可以先提交批次，再决定何时运行。下面在仓库根目录执行；将 `$session` 换成 `/sessions` 显示的完整会话 ID，使用自定义数据目录时给每次调用加上相同的全局 `--data-dir`。

```powershell
$session = '替换为会话ID'
@{ branches = @(@{ question = '令牌刷新在哪里处理？'; path = 'src/mcp'; query = 'invalid_grant' }) } |
    ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 first.json
$a = coda --json jobs --session $session submit --key refresh-impl --file first.json | ConvertFrom-Json

@{ branches = @(@{ question = '哪些测试覆盖这一行为？'; path = 'tests'; query = 'invalid_grant'; depends_on = @($a.jobs[0].id) }) } |
    ConvertTo-Json -Depth 5 | Set-Content -Encoding UTF8 next.json
$c = coda --json jobs --session $session submit --key refresh-tests --file next.json | ConvertFrom-Json
coda jobs --session $session continue $c.id
```

`submit` 返回批次和子任务 ID；相同提交键和输入在同一会话内可重复使用。`continue` 先推进指定任务所需的前置分支，再读取它们已保存的摘要和证据。依赖必须来自同一项目、工作区和会话内已提交的批次。进程退出后可再次运行同一条命令。[Sprint 05](docs/sprints/SPRINT-05-parallel-exploration.md#s5-t2)

- **工程级内建工具集**：
  - `.gitignore` 感知的高速检索：`glob_files`（递归通配发现）与 `search_files`（正则/字面量及上下文行搜索）
  - `explore_files` 可把关于 1–4 个指定源码文件的问题交给一次独立模型请求。每个文件最多取前 800 行及 8 KiB 的带行号文本，主会话收到最多 4 KiB 的摘要。模型请求沿用当前任务的路由、网络授权和预算。例如可运行 `coda ask "请用 explore_files 阅读 src/store.rs 和 src/engine.rs，解释任务如何恢复，引用相关行号。"`。
  - 精准读写与补丁编辑：`read_file`（带行号分页读取）、`edit_file`（支持单处精确替换、`edits` 批量替换与多 Hunk Unified `patch` 模式，自动适配 CRLF/LF 并生成行级 Diff）、`write_file`（原子写盘）
  - 仓库状态、任务规划与受控终端：`git_status`（结构化工作区变更洞察）、`todo_write` / `todo_read`（多步工程任务清单追踪）、`run_command`（Windows JobObject 受控 PowerShell 子进程树回收）
- **标准 MCP（Model Context Protocol）stdio 生态**：
  - 自动发现工作区 `.mcp.json` 或 `.coda/mcp.json` 中配置的本地 MCP Server，通过标准 JSON-RPC 2.0 stdio 协议动态注册外部工具（`mcp__<server>__<tool>`），并统一纳入宿主机审批与安全审计闭环。同一 Engine 中的同一会话复用服务连接和工具定义，工具可在多轮调用间保留进程内状态。
- **副作用可审阅、可回滚与长上下文韧性**：
  - 文件写入、外部命令与 MCP 调用均支持行级预览与细粒度审批；所有文件变更具备持久化前镜像备份（`/undo` 与 Checkpoint 恢复），自动加载项目 `AGENTS.md` / `CLAUDE.md` 规范并在长链路推理中保持上下文自动压缩与中断可恢复。

### 在 Windows 等待记事本的保存框

在已登录的交互桌面打开记事本。用下面的命令查看有窗口的记事本进程，再把 `$targetPid` 换成自己要观察的 PID。以下示例在工作区根目录的 PowerShell 中执行。

```powershell
Get-Process notepad | Where-Object MainWindowHandle -ne 0 | Select-Object Id, MainWindowTitle
$targetPid = 12345
$env:CODA_COMPUTER_TARGET = @{ workspace = (Resolve-Path .).Path; pid = $targetPid } | ConvertTo-Json -Compress
coda --workspace . --json computer watch --timeout-ms 60000
```

`watch` 在本机逐行输出 JSON，无需模型凭据。在 60 秒内切到记事本、手动打开“另存为”、切走再切回，可观察 `target_foreground`、`dialog_foreground`、`outside_target` 的转换。超时、目标进程退出或 Ctrl+C 会结束观察。标准对话框由登记进程的前台窗口类识别；记事本版本若使用自绘或其他进程的窗口，以实际状态为准。

使用同一环境启动 `coda chat`、`coda ask` 或 `coda serve`，允许工具的任务在匹配工作区可调用 `computer_observe` 和 `computer_wait`。例如让模型“调用 computer_wait，等我手动打开目标应用的标准对话框，超时 30000 毫秒，告诉我状态”。等待由 Host 处理。目标窗口的标题和类名会进入会话并发送给当前模型。切到其他进程只报告焦点离开。观察引用限当前 Host 和会话。结束后关闭启用该能力的 Host，并在 PowerShell 运行 `Remove-Item Env:CODA_COMPUTER_TARGET`。[本轮范围与后续控件方案](docs/sprints/SPRINT-06-windows-computer.md)

### 接入远程 MCP 工具

在启动 `coda ask`、`coda chat` 或 `coda serve` 的进程环境中设置 `CODA_MCP_HTTP_SERVERS`。每一项登记一个工作区的服务。下面的 PowerShell 示例使用当前目录，运行前将 `url` 换成服务的实际 MCP 端点。

```powershell
$mcpServer = @{
    workspace = (Resolve-Path .).Path
    name = 'docs'
    url = 'https://mcp.example.org/mcp'
    bearerTokenEnv = 'DOCS_MCP_TOKEN'
}
$env:CODA_MCP_HTTP_SERVERS = ConvertTo-Json -InputObject @($mcpServer) -Compress
coda chat
```

需要 Bearer 认证时，在同一进程环境中提供 `DOCS_MCP_TOKEN`；匿名服务可省略 `bearerTokenEnv`。端点使用 HTTPS，本机调试也可使用 `http://127.0.0.1` 或 `http://[::1]`。URL 中的用户名、密码、查询参数和片段会使登记无效。同一工作区内服务名应唯一，可使用字母、数字、下划线和连字符。

使用 OAuth 时，请在授权服务器登记公共客户端，允许回调到 `http://127.0.0.1:<端口>/callback`。在同一 PowerShell 窗口运行以下示例，将客户端 ID、发行者和权限范围换成服务实际提供的值。

```powershell
$mcpServer.Remove('bearerTokenEnv')
$mcpServer.oauth = @{
    clientId = 'coda-public'
    issuer = 'https://auth.example.org'
    scopes = @('docs.read')
}
$env:CODA_MCP_HTTP_SERVERS = ConvertTo-Json -InputObject @($mcpServer) -Depth 4 -Compress
coda mcp login docs --dir . --callback-port 8173
coda chat --dir .
```

在本机浏览器打开终端显示的地址并完成授权。示例使用预登记端口 8173。支持动态回环端口的服务可以省略 `--callback-port`。按 Ctrl+C 取消登录。已在 `coda chat` 中时可直接输入 `/mcp login docs --callback-port 8173`。Windows 会用 DPAPI 保存凭据，后续相同工作区、服务登记与数据目录中的请求可以复用凭据。使用独立 Host 时先登录，再用同一数据目录启动，例如 `coda mcp login docs --data-dir C:\coda-data` 和 `coda --data-dir C:\coda-data serve`。

服务需提供与登记资源匹配的受保护资源元数据、与 `issuer` 匹配的授权服务器元数据，以及 S256 PKCE 支持。资源挑战中的元数据地址须与 MCP 资源同源。授权服务器可在元数据中公布不同 HTTPS 域名的授权及令牌端点。如果服务要求特定权限范围才签发刷新令牌，请在 `scopes` 中登记该范围。获得刷新令牌后，CLI 会在访问令牌到期或发现工具时收到 401 后尝试刷新。授权失效时，终端会提示重新登录。已派发的工具调用收到 401 时会记录结果未知，避免重复执行。

工具沿用 `mcp__<服务名>__<工具名>` 命名。工作台在审批时显示本次调用绑定的地址。同一会话可复用工具目录和连接；派发后的响应中断会记录为结果未知。支持 POST 返回 JSON 或 SSE，并处理分页工具目录与服务端会话 ID。[MCP 传输协议 2025-06-18](https://modelcontextprotocol.io/specification/2025-06-18/basic/transports)
