# NoHumanCode 项目书

仓库可克隆到任意位置；本文正文中的 `./` 指仓库根，链接相对于所在文件。

版本：1.0（Stage 1 交付基线）
日期：2026-11-18
状态：Stage 1 CLI AI 编程工作台已交付，Desktop GUI 延后至第二阶段

## 现行状态导航（2026-11-18 基线）

本书维护 **NoHumanCode** 的长期产品目标、核心设计理念与两阶段演进原则。现行代码位置、执行调用链、全部 38 个源码模块边界与自动化测试矩阵，统一以根目录 **[PROJECT-MAP.md](PROJECT-MAP.md)** 为唯一权威导航；日常开发与验证准则以 **[AGENTS.md](AGENTS.md)** 为准；快速上手命令见 **[README.md](README.md)**。

当前主分支（`main`）核心基线事实：

- **源码与协议结构**：Git 根目录为 `./`，Rust 工作区位于 [NoManCode/rust-app](NoManCode/rust-app)（宿主与 CLI 二进制兼容标识 `peachsh`，零 I/O 共享协议 crate 为 [NoManCode/rust-app/crates/protocol](NoManCode/rust-app/crates/protocol)）。
- **数据库与持久化**：SQLite WAL 模式，`SCHEMA_VERSION = 18`，完整落地 Project / Session / Turn / Agent / Checkpoint 领域仓储、事件游标流、幂等回执与中断自动恢复。
- **Stage 1 核心闭环已交付**：进程内零仪式感 CLI 工作台（`peachsh init / chat / ask`）、三大主流协议原生流式直连（OpenAI / Anthropic Messages / Google Gemini）、行级彩色 Myers Diff 预览与 `[y/n/a]` 审批、前镜像备份回滚（`/diff` `/undo` `/checkpoint` `/restore`）、标准 MCP Stdio 工具动态接入及 Git Worktree 隔离管理均已稳定交付并通过全量门禁测试。
- **架构精简与历史归档**：图形界面（Desktop GUI）明确延后至第二阶段；空壳 `crates/ui`、死代码 `src/wasm*`、过度复杂的伪插件目录层以及早期多角色协作与审批文书目录 `niuma/` 均已从 `main` 清理移除（保留 `NoManCode/rust-app/web/` 作为 `server.rs` 内嵌静态兜底页）。早期历史文书与阶段性工作树完整归档于 Git 分支 `archive/gpt-era-20261118`、`archive/old-c35-20261118` 与 `archive/u18-consolidated-20261118`。

*(注：下文第 1～5 节完整保留项目的长期产品哲学与设计初衷；其中涉及 2026-09 早期快照及旧插件拆分的局部细节仅供演进背景参考，现行实现结构请以 [PROJECT-MAP.md](PROJECT-MAP.md) 为准。)*

## 1. 项目定位

NoHumanCode 是 Windows 本机优先的 AI 编程工作台。它要帮助用户完成一条可审阅、可恢复、可继续的开发闭环：

```text
选择项目 → 提出目标 → 读取代码 → 获得操作批准
→ 修改/运行测试 → 查看事实和 diff → 接受、恢复或继续
→ 重启后继续同一会话
```

产品的核心不是“调用模型”，而是提供一个可以组合、替换、审阅、恢复和持续优化的开发环境。

我们借鉴 DeepSeek Harness、Cordis 和时空可组合性理论，但不把任何现有项目当作终点。它们是第一批设计证据；NoHumanCode 将根据 Windows、本机 AI 编程、审批、凭据、Diff、Checkpoint 和多模型协作的实际需求逐步优化。

## 2. 现行事实与历史快照

2026-11-18 的现行事实：Git 总根是 `./`，源码在 [NoManCode/rust-app](NoManCode/rust-app)，代码中的 `SCHEMA_VERSION` 为 `18`，Stage 1 CLI AI 编程工作台与本地 HTTP/SSE Host 契约已完整交付。早期 2026-09-23 快照（当时 `SCHEMA_VERSION` 为 6，处于分模块基础建设期）已演进收敛为当前主干。

实际模块状态与测试覆盖统一依据当前代码与 [PROJECT-MAP.md](PROJECT-MAP.md) 核对。

### 2.1 2026-09-22 旧实例快照（仅供查证）

下列是旧目录当时的状态，schema 5、无 Git、Repository 尚未完成等描述已被上方当前事实更新，不能作为现行开工指令。当时参考文件为：

- `./.local/legacy-instance/HANDOFF-TO-NEXT-MODEL.md`
- `./.local/legacy-instance/rust-app\MASTER-ARCHITECTURE-BASELINE-2026-09-22.md`

当时状态：

- Rust Host 已运行在 `http://127.0.0.1:3090/`；
- 健康接口当前为 Rust runtime、schema 5、version 0.2.0；
- `data-rust` 是新版数据目录，`data` 是旧 Node 数据来源，`workspace` 是项目工作区；
- `crates/protocol` 已有 `SessionKind` 和第一版结构化错误；
- `crates/ui` 是可编译的 Leptos CSR/WASM 迁移壳；
- 实际可用页面仍是 `web/index.html`、`web/app.js` 和 `web/style.css`；
- 当前目录不是 Git 仓库，不能假设存在 commit、分支或可用 diff；
- 旧 Node/3080 入口保留为迁移回退，不代表 Rust 版本状态。

尚未完成的能力包括完整 Leptos 页面、Project/Session/Turn repository、逐次审批界面、Diff/Checkpoint、shared/worktree 产品化隔离、持久 Scheduler、KeyPool、完整 Team、MCP/Skills 和旧会话一比一迁移。

## 3. 总体设计理念

### 3.1 Host 是稳定的上下文、权限和生命周期中心

Rust Host 负责：

- 运行时上下文和插件注册表；
- Project、Session、Turn、Agent、Task、Workspace；
- 持久事件、状态投影、迁移和恢复；
- Provider、模型、Key、预算和调用记录；
- 文件、Git、Shell、外部进程和工作区；
- Approval、Capability 和安全策略；
- 插件加载、卸载、失败隔离和恢复。

CLI、Desktop 和插件都不能绕过 Host 直接访问 Key、SQLite、文件系统或进程。

### 3.2 插件是可组合、可撤销、可替换的组件

插件必须声明：

```text
身份、版本、依赖、提供项、权限、配置、作用域、生命周期和支持的运行面
```

插件产生的服务注册、事件监听、缓存、子任务和其他副作用都必须可追踪。卸载插件时撤销这些效果；无法撤销的副作用必须进入显式恢复或人工处理流程。

### 3.3 依赖显式化、版本化和可诊断

插件通过版本化的 service、command、query、event、resource 和 capability 依赖其他能力。禁止通过成员显示名、特殊字符串或隐式全局变量建立关键依赖。

运行时应能报告：缺失依赖、版本不兼容、依赖循环、提供方替换和作用域冲突。

### 3.4 副作用可审阅

读取项目范围内文件默认允许。写入、命令、额外网络、Git 合并、MCP 和其他外部副作用需要逐次审批。

插件权限声明不等于自动授权。实际调用必须经过 Host capability policy 和审批状态。

### 3.5 UI 是表现层，不是事实来源

CLI 和 Desktop 共享同一套 Host、协议、事件、数据和权限结果。UI 只发送 typed action，读取 snapshot，订阅事件。

插件 UI 先采用声明式贡献：

```text
command / panel / form / message block / approval card / diff renderer / status widget
```

插件不能直接修改主页面 DOM、持有领域事实或绕过 Host 执行副作用。

## 4. 目标架构

```text
                    ┌──────────────────────┐
                    │  CLI 开发者工作台     │
                    └──────────┬───────────┘
                               │ protocol / events
┌──────────────────────┐       ▼        ┌──────────────────────┐
│ Desktop 友好界面     │◄──► Rust Host ◄─┤ Plugin Runtime       │
│ Leptos CSR/WASM      │       │        │ manifest/lifecycle   │
└──────────────────────┘       │        └──────────────────────┘
                               │
       ┌───────────────────────┼────────────────────────┐
       ▼                       ▼                        ▼
  Domain State            Capability Policy        Persistence
  Project/Session         Approval/Permissions     SQLite/Event Log
  Turn/Task/Workspace     Provider/Key boundary    Recovery/Migration
```

插件运行形态按信任和隔离需求分层：

- 内置 Rust 插件：高信任、核心能力；
- WASM 插件：受限、可移植、适合第三方扩展；
- 外部进程插件：适合其他语言、大工具链和更强故障隔离。

所有形态都通过版本化协议和 Host capability gateway 工作。

## 5. 两阶段产品路线

### 阶段一：CLI 开发者版，后端优先

CLI 是开发者使用的第一阶段工作台，也是 Host 和插件系统的参考客户端。它不是另一套业务逻辑。

第一阶段必须先完成单 Agent 的开发闭环：

```text
选择项目 → 创建 Session → 模型对话 → 读取/搜索代码
→ 请求写入/命令审批 → 修改并测试
→ 查看事件和 Diff → 接受、恢复或继续
```

后端优先交付顺序：

1. Plugin manifest、Context、Registry、Dependency Resolution；
2. 可撤销 effect、插件生命周期、卸载和失败隔离；
3. Project/Session/Turn 的持久化 repository 和事件日志；
4. Capability、Approval、ToolCall 和权限策略；
5. Provider、模型调用、Key 命名空间、预算和错误分类；
6. Workspace、文件工具、Shell、Git Diff、Checkpoint 和恢复；
7. CLI 命令、JSON 输出、事件流、重连、取消和诊断；
8. 默认插件组合和开发者验收场景。

阶段一的第一版默认插件：

```text
core
session-log
local-project
workspace
provider-xpeach
file-read-search
approval-policy
file-write
shell
git-diff
checkpoint-restore
recovery
chat-agent
```

阶段一验收必须证明：

- Session 身份与成员显示名无关；
- 重复请求不会重复创建任务或重复副作用；
- SSE/事件断线可从最后游标继续；
- 停止、失败和重启不会丢失草稿或伪造完成；
- 写入、命令和网络都经过审批；
- Key 不出现在 CLI 输出、日志、事件或浏览器接口；
- 插件禁用/卸载后不残留注册项、监听器和后台任务；
- 旧 HTML/JS fallback 仍然可以启动；
- `fmt`、`check`、`test`、`clippy`、`wasm-check` 继续通过。

完整 Team、MCP、Skills、复杂上下文压缩和高级 KeyPool 在单 Agent 闭环稳定后逐步增加，不在第一轮同时实现。

### 阶段二：Desktop 友好版

Desktop 使用阶段一已经稳定的 Host 和协议，提供更适合日常使用的图形界面。当前前端方向为 Leptos CSR/WASM；具体桌面外壳在协议和 UI 扩展边界稳定后再决定。

第二阶段交付：

- Project、Session、Chat、Turn 页面；
- 流式消息、失败草稿、停止、继续和断线重连；
- 审批卡片、工具调用详情和权限范围；
- 文件树、逐文件/逐行 Diff、Checkpoint 和恢复；
- Team 任务、成员状态和结果汇总；
- 插件面板、消息块、设置卡片和状态扩展；
- 浏览器 E2E、重启继续和凭据不泄漏验证。

Desktop 不重新实现领域逻辑，也不创建另一套数据格式。CLI 保留为开发者、诊断和恢复入口。

## 6. 多模型协作架构

### 6.1 GPT-6：项目总监 / 架构与验收负责人

职责：

- 维护项目目标、边界和优先级；
- 将需求拆成可独立验收的后端、协议和前端任务；
- 审查领域对象、插件边界、API、事件和 schema 迁移；
- 决定是否接受实现、退回修改或调整路线；
- 维护项目书、交接文档、决策记录和风险清单；
- 运行最终集成验收，确认没有把计划写成已完成。

不应替后端或前端模型盲目扩大任务范围。所有跨模块变化先由项目总监确认契约。

### 6.2 Grok 4.7：后端实现负责人

职责：

- Rust Host、Domain、Store、Protocol、Engine 和 Server；
- Plugin Runtime、Context、Registry、Dependency Resolution；
- 生命周期、可撤销 effect、失败隔离和恢复；
- Capability、Approval、ToolCall、Provider、Workspace、Git 和 CLI；
- SQLite repository、事件日志、迁移和幂等；
- 单元测试、集成测试、安全测试和性能诊断。

后端实现负责人不直接决定产品需求，不把前端临时需求写进领域模型，不以成员名称或 UI 状态替代稳定 ID 和状态机。

### 6.3 Kimi K3：前端实现负责人

职责从后端协议稳定后开始：

- Leptos CSR/WASM 页面和状态投影；
- CLI/Host 协议的可视化验证页面；
- Chat、Approval、ToolCall、Diff、Checkpoint 和 Session 页面；
- 声明式插件 UI slot 和 renderer；
- SSE 重连、失败草稿、重启继续和浏览器 E2E；
- 确认浏览器不接触 Key、DPAPI、文件、进程和 SQLite。

前端实现负责人可以在后端阶段提前制作 mock client 和协议测试，但不能自行发明未确认的 API、事件或权限语义。

## 7. 协作协议

每个模型提交工作时必须报告：

```text
目标和范围
修改的文件
领域对象变化
API / Event / Protocol 变化
Schema / Migration 变化
权限和凭据影响
失败、取消、卸载和恢复路径
测试命令和结果
未完成事项与风险
```

跨模型修改必须先写出契约，再写代码。后端先提供可测试的 API、事件和 mock；前端根据已确认的契约实现，不以临时字段反向锁定后端。

当前已有 Git 仓库，根为 ./。有效修复和目录迁移仍含未提交内容；新工作树若只从 HEAD 创建，不包含这些成果。并行修改前由经理明确代码基线、文件唯一写入人及隔离方式，不允许多个员工同时改同一区域，也不因旧路径显示删除就还原或清理新目录。

## 8. 不可违反的边界

- 不索要、输出或猜测任何真实 API Key；
- 浏览器永远不接触 Key、DPAPI 明文、文件系统、进程句柄或 SQLite；
- 读取默认允许，写入、命令、网络和 Git 合并逐次审批；
- shared/worktree 由用户选择，缺少 Git 条件时不得静默降级；
- 失败或中断不能伪造 completed；
- 未经测试和实机证据的能力只能标记为计划中；
- 不把 DSH、Cordis 或论文的设计当成无需验证的实现保证；
- 不同时重写 Team、Provider、数据库和全部 UI；
- 保持旧 HTML/JS fallback，直到新链路完成验收。

## 9. 历史协作归档与当前演进指引

*(注：第 6～7 节描述的多角色人工分发流程及原 `niuma/` 目录已于 2026-11-18 退役归档至 `archive/gpt-era-20261118` 分支。当前开发直接遵循 [AGENTS.md](AGENTS.md) 与 [PROJECT-MAP.md](PROJECT-MAP.md)。)*

### 当前下一步（Stage 2+ 演进方向）

Stage 1 后端 Host 与进程内 CLI AI 编程工作台（多模型原生流式、内建编码工具集、标准 MCP Stdio、行级 Diff 审批、Checkpoint/Restore 与 Worktree 管理）已在 `main` 完整交付并通过全部自动化门禁。

后续演进按照小步可验证切片推进（详见 [PROJECT-MAP.md 第 5 节](PROJECT-MAP.md#5-当前已完成边界-vs-后续演进方向)）：

1. 持续打磨日常 CLI 交互细节、远程 HTTP/SSE MCP Transport 扩展与多账号 KeyPool 故障转移；
2. 在 `peachsh-protocol` 与本地 HTTP/SSE Host 稳定契约之上，开展第二阶段原生桌面图形界面（Desktop GUI）设计，不重复实现领域逻辑，也不破坏现有 CLI 零仪式感体验。
