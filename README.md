# NoHumanCode

Windows 本机优先的旗舰级 AI 编程工作台：聚焦**多模型原生流式、可审阅、可回滚、可恢复、零仪式感**的 CLI 编码闭环。

## 目录结构

- `NoManCode/rust-app`：核心 Rust 工程（二进制名保留兼容 `peachsh`，含共享协议 `crates/protocol`）
- `references/`：架构与同类产品参考资料

## 快速开始

### 1. 构建

```powershell
cd NoManCode/rust-app
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action build
```

### 2. 模型路由与多厂商原生流式凭据（支持 Windows DPAPI 加密存储或零初始化环境变量）

原生支持三大主流协议的流式推理与工具调用，无需额外部署中转层：

- **OpenAI Chat Completions**（及兼容接口）：`OPENAI_API_KEY`（可选 `OPENAI_BASE_URL`、`NHC_MODEL`）
- **Anthropic Messages API**（原生支持 Extended Thinking + Tool Use）：`ANTHROPIC_API_KEY`
- **Google Gemini `streamGenerateContent`**：`GEMINI_API_KEY`

```powershell
# 方式 A：一步式持久化初始化路由与加密凭据
peachsh init --base-url https://api.openai.com/v1 --model gpt-4.1

# 方式 B：零初始化（Zero-Init）环境变量直连开箱即用
$env:OPENAI_API_KEY = "sk-..."          # 或设置 ANTHROPIC_API_KEY / GEMINI_API_KEY
$env:OPENAI_BASE_URL = "https://api.openai.com/v1"
$env:NHC_MODEL = "gpt-4.1"
```

### 3. 交互式工作台（`peachsh chat`）与单次执行（`peachsh ask`）

```powershell
# 启动交互式流式 REPL（支持行级彩色 Diff 预览与单键 [y/n/a] 审批）
peachsh chat

# 无缝续接当前目录最近一次会话，或指定 session_id / run_id 恢复多轮上下文
peachsh chat -c
peachsh chat --resume <session_or_run_id>

# 单次执行编码任务（同样支持 -y 自动审批、-c / --continue 跨调用续接与 @ 内联引用）
peachsh ask -y "解释 @src/lib.rs:1-40 并梳理 @src/ 目录职责"
peachsh ask -c "基于刚才的分析补充边界单元测试并运行验证"
```

#### 内联上下文引用与 REPL 斜杠命令

- **内联工作区引用**：在提问中直接写入 `@path/to/file`、`@path/to/file:start-end`（精确行范围）或 `@dir/`（目录清单），自动安全解析并注入带行号的 `<context_files>` 上下文。
- **交互式斜杠命令**：
  - `/sessions`：列出当前项目近期会话（ID、状态、轮数、标题）
  - `/resume <id>`：即时切换并续接指定历史会话
  - `/diff`：查看当前工作区 Git 状态、变更统计与本轮已追踪文件修改
  - `/undo`：一键回滚最近一轮任务产生的文件修改
  - `/checkpoint` & `/restore <checkpoint_id>`：为当前任务创建持久化快照检查点并支持随时精确恢复
  - `/compact`：按原子工具调用组压缩长对话历史消息并展示节省体积
  - `/status` / `/model <name>` / `/auto` / `/new`：查看会话与 Token 用量统计（每轮结束自动展示 `[tokens: prompt=X, completion=Y, total=Z]`）、切换模型、切换自动审批或开启新会话

### 4. 启动本地 HTTP/SSE Host 服务（供脚本或远程客户端调用）

```powershell
peachsh --port 3090 --data-dir .nhc-data --workspace .
```

## 核心编码工具与 MCP 生态

- **工程级内建工具集**：
  - `.gitignore` 感知的高速检索：`glob_files`（递归通配发现）与 `search_files`（正则/字面量及上下文行搜索）
  - 精准读写与补丁编辑：`read_file`（带行号分页读取）、`edit_file`（支持单处精确替换、`edits` 批量替换与多 Hunk Unified `patch` 模式，自动适配 CRLF/LF 并生成行级 Diff）、`write_file`（原子写盘）
  - 仓库状态、任务规划与受控终端：`git_status`（结构化工作区变更洞察）、`todo_write` / `todo_read`（多步工程任务清单追踪）、`run_command`（Windows JobObject 受控 PowerShell 子进程树回收）
- **标准 MCP（Model Context Protocol）stdio 生态**：
  - 自动发现工作区 `.mcp.json` 或 `.nhc/mcp.json` 中配置的本地 MCP Server，通过标准 JSON-RPC 2.0 stdio 协议动态注册外部工具（`mcp__<server>__<tool>`），并统一纳入宿主机审批与安全审计闭环。
- **副作用可审阅、可回滚与长上下文韧性**：
  - 文件写入、外部命令与 MCP 调用均支持行级预览与细粒度审批；所有文件变更具备持久化前镜像备份（`/undo` 与 Checkpoint 恢复），自动加载项目 `AGENTS.md` / `CLAUDE.md` 规范并在长链路推理中保持上下文自动压缩与中断可恢复。
