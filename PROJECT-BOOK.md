# Coda CLI 项目书

仓库可克隆到任意位置；本文正文中的 `./` 指仓库根，链接相对于所在文件。

版本：1.0（Stage 1 交付基线）
日期：2026-11-18
状态：Stage 1 Coda CLI 编程工作台已交付，Desktop GUI 延后至第二阶段

## 现行状态导航（2026-11-18 基线）

本书维护 **Coda CLI** 的长期产品目标、核心设计理念与两阶段演进原则。现行代码位置、执行调用链、各源码模块的边界与自动化测试矩阵，统一以根目录 **[PROJECT-MAP.md](PROJECT-MAP.md)** 为唯一权威导航；日常开发与验证准则以 **[AGENTS.md](AGENTS.md)** 为准；快速上手命令见 **[README.md](README.md)**。

当前主分支（`main`）核心基线事实：

- **源码与协议结构**：Git 与 Rust Workspace 均直接位于仓库根目录 `./`（宿主库与 CLI 二进制名为 `coda`，零 I/O 共享协议 crate 为 [crates/protocol](crates/protocol) `coda-protocol`）。
- **数据库与持久化**：默认数据目录 `%LOCALAPPDATA%\coda`（可通过 `CODA_DATA_DIR` 覆盖），SQLite WAL 数据库 `coda.sqlite3`（`SCHEMA_VERSION = 19`），完整落地 Project / Session / Turn / Agent / Checkpoint 领域仓储、事件游标流、幂等回执与中断自动恢复。
- **Stage 1 核心闭环已交付**：进程内零仪式感 CLI 工作台（`coda init / chat / ask`）、三大主流协议原生流式直连（OpenAI / Anthropic Messages / Google Gemini，支持 `CODA_MODEL` 路由覆盖）、行级彩色 Myers Diff 预览与 `[y/n/a]` 审批、前镜像备份回滚（`/diff` `/undo` `/checkpoint` `/restore`）、标准 MCP Stdio 工具（`.mcp.json` / `.coda/mcp.json`）动态接入及 Git Worktree 隔离管理均已稳定交付并通过全量门禁测试。
- **架构精简与历史归档**：图形界面（Desktop GUI）明确延后至第二阶段；早期嵌套源码目录、空壳 `crates/ui`、死代码 `src/wasm*`、过度复杂的伪插件目录层以及早期多角色文书目录均已从 `main` 清理移除（保留 [web/](web) 作为 `server.rs` 内嵌静态兜底页）。早期历史归档分支 `archive/gpt-era-20261118`、`archive/old-c35-20261118` 与 `archive/u18-consolidated-20261118` 保留了重命名前的早期实验代号快照。

*(注：下文第 1～5 节完整保留项目的长期产品哲学与设计初衷；现行模块实现与目录结构请以 [PROJECT-MAP.md](PROJECT-MAP.md) 为准。)*

## 1. 项目定位

**Coda CLI** 是 Windows 本机优先的 AI 编程工作台。它要帮助用户完成一条可审阅、可恢复、可继续的开发闭环：

```text
选择项目 → 提出目标 → 读取代码 → 获得操作批准
→ 修改/运行测试 → 查看事实和 diff → 接受、恢复或继续
→ 重启后继续同一会话
```

产品的核心不是“调用模型”，而是提供一个可以组合、替换、审阅、恢复和持续优化的开发环境。

我们借鉴现代 Agent Harness、Cordis 和时空可组合性理论，但不把任何现有项目当作终点。它们是第一批设计证据；Coda CLI 将根据 Windows、本机 AI 编程、审批、凭据、Diff、Checkpoint 和多模型协作的实际需求逐步优化。

## 2. 现行事实与演进背景

2026-11-18 的现行事实：仓库根 `./` 即为 Rust Workspace，代码中的 `SCHEMA_VERSION` 为 `19`，Stage 1 **Coda CLI**（`coda`）编程工作台与本地 HTTP/SSE Host 契约（含 `x-coda-token` 鉴权）已完整交付。早期原型阶段（`SCHEMA_VERSION` 5～6 时期）的基础设施已全部收敛至根目录主干。

实际模块状态与测试覆盖统一依据当前代码与 [PROJECT-MAP.md](PROJECT-MAP.md) 核对。

## 3. 总体设计理念

### 3.1 Host 是稳定的上下文、权限和生命周期中心

Rust Host（`coda`）负责：

- 运行时上下文和扩展工具注册表；
- Project、Session、Turn、Agent、Task、Workspace；
- 持久事件、状态投影、迁移和恢复；
- Provider、模型、Key、预算和调用记录；
- 文件、Git、Shell、外部进程和工作区；
- Approval、Capability 和安全策略；
- 外部工具（如 MCP Server）加载、卸载、失败隔离和恢复。

CLI、未来的 Desktop 和外部工具都不能绕过 Host 直接访问 Key、SQLite、受保护工作区或未授权进程。

### 3.2 扩展组件是可组合、可撤销、可替换的边界

扩展能力遵循显式声明原则：

```text
身份、版本、依赖、提供项、权限、配置、作用域、生命周期和支持的运行面
```

扩展产生的服务注册、事件监听、缓存、子任务和其他副作用都必须可追踪；无法自动撤销的副作用必须进入显式恢复或检查点（Checkpoint）回退流程。

### 3.3 依赖显式化、版本化和可诊断

核心服务通过明确类型的协议、命令、查询、事件和权限边界协同工作，禁止通过显示名称、脆弱字符串拼接或隐式全局状态建立关键依赖。

### 3.4 副作用可审阅

读取项目范围内文件默认允许。写入、命令、额外网络、Git 变更、MCP 调用和其他外部副作用默认经过行级预览与逐次审批（或用户显式开启会话级自动放行）。

### 3.5 UI 是表现层，不是事实来源

`coda` CLI 和未来的 Desktop 客户端共享同一套 Host、协议（`coda-protocol`）、事件、数据和权限结果。客户端发送明确类型的动作，读取快照，订阅事件，不自行伪造领域状态。

## 4. 目标架构

```text
                     ┌──────────────────────┐
                     │  Coda CLI 编程工作台  │
                     └──────────┬───────────┘
                                │ in-process / protocol / events
┌──────────────────────┐       ▼        ┌──────────────────────┐
│ Desktop 友好界面     │◄──► Coda Host ◄┤ 工具与 MCP 生态      │
│ (Stage 2 规划)       │       │        │ workspace / stdio    │
└──────────────────────┘       │        └──────────────────────┘
                                │
        ┌───────────────────────┼────────────────────────┐
        ▼                       ▼                        ▼
   Domain State            Capability Policy        Persistence
   Project/Session         Approval/Permissions     SQLite/Event Log
   Turn/Task/Workspace     Provider/Key boundary    Recovery/Migration
```

## 5. 两阶段产品路线

### 阶段一：Coda CLI 开发者版，后端优先（已交付）

`coda` CLI 是开发者日常使用的主力第一阶段工作台，也是 Host 的直接入口。

第一阶段核心开发闭环：

```text
选择项目 → 创建/恢复 Session → 多模型流式对话 → 读取/搜索代码
→ 预览彩色 Diff 并请求写入/命令审批 → 修改并运行测试
→ 查看事件和 Diff → 接受、/undo 回退、Checkpoint 恢复或继续
```

阶段一核心验收保障：

- Session 身份与成员显示名无关；
- 重复请求通过幂等回执避免重复创建任务或重复副作用；
- SSE/事件断线可从最后游标继续；
- 停止、失败和重启不会丢失草稿或伪造 `completed`（未决运行收敛为 `interrupted`）；
- 文件写入、终端命令和外部 MCP 调用均支持细粒度审批；
- API Key 经 Windows DPAPI 加密存储，不出现在 CLI 输出、日志、事件或 HTTP 响应中；
- `fmt`、`check`、`test`、`clippy`、`build` 全量门禁持续通过。

### 阶段二：Desktop 友好版（后续规划）

Desktop 将复用阶段一已经稳定的 `coda` Host 和 `coda-protocol` 契约，提供图形化多文件审阅与多会话管理界面：

- Project、Session、Chat、Turn 可视化视图；
- 流式消息、失败草稿、停止、继续和断线重连；
- 审批卡片、工具调用详情和权限范围；
- 文件树、逐文件/逐行 Diff、Checkpoint 时间线和恢复；
- 多任务状态看板与结果汇总。

Desktop 不重新实现领域逻辑，也不创建另一套数据格式；`coda` CLI 始终作为核心高频编码、自动化与诊断入口。

## 6. 不可违反的工程与安全边界

- 不索要、输出或猜测任何真实 API Key；
- 外部客户端永远不直接接触 Key 明文、DPAPI 密文、底层进程句柄或 SQLite 文件；
- 读取默认允许，写入、命令、外部网络和 MCP 副作用纳入审批闭环；
- Worktree 隔离由用户明确控制，缺少 Git 条件时不得静默降级覆盖主工作树；
- 失败或中断绝不伪造 `completed`；
- 未经测试验证的能力只能标记为规划中。

## 7. 历史归档与当前演进指引

*(注：项目早期阶段的多角色协作实验记录与旧目录包装已于 2026-11-18 完整归档至 `archive/*` 历史分支。当前开发统一遵循 [AGENTS.md](AGENTS.md) 与 [PROJECT-MAP.md](PROJECT-MAP.md)。)*

### 当前下一步（Stage 2+ 演进方向）

Stage 1 后端 Host 与进程内 **Coda CLI** 工作台已在 `main` 完整交付。后续演进按照小步可验证切片推进（详见 [PROJECT-MAP.md 第 5 节](PROJECT-MAP.md#5-当前已完成边界-vs-后续演进方向)）：

1. 持续打磨 `coda` 日常 CLI 交互细节、远程 HTTP/SSE MCP Transport 扩展与多账号 KeyPool 故障转移；
2. 在 `coda-protocol` 与本地 HTTP/SSE Host 稳定契约之上，开展第二阶段原生桌面图形界面（Desktop GUI）设计，不重复实现领域逻辑，也不破坏现有 CLI 零仪式感体验。
