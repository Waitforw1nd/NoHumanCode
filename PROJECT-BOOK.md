# NoHumanCode 项目书

仓库可克隆到任意位置；本文正文中的 `./` 指仓库根，链接相对于所在文件。全部协作者遵循 [路径与可移植性](路径与可移植性.md)，历史本机路径已按用户要求相对化，旧哈希对应改写前证据。

版本：0.2
日期：2026-09-23
状态：多模型协作开发基线

## 当前执行说明（2026-09-23）

本轮最新交接：C-R2-01 第2.1轮补证经 [review 2.1](<niuma/员工C/审查记录/第二轮/员工C review 2.1.md>) APPROVED（NEXT-01 范围）；D-R1-01 已由用户指定 SWE2max 完成并在实现与冻结快照范围 APPROVED。C/D 联合候选已由经理串行执行门禁通过（[记录](niuma/项目经理/审查记录/2026-09-23C-D联合门禁.md)）；均未 Git 提交/合并/发布，Git 基线收尾待执行。SWE2max仅覆盖D本任务的默认模型安排，C原安排保持。范围、源码唯一所有权及固定快照验证见 [双线协调](niuma/项目经理/任务/2026-09-23C-D双线协调.md)，实际执行状态以任务板/员工身份为准。

新对话接任项目经理，从 [启动说明](niuma/项目经理/启动说明.md) 开始；现行代码位置与调用链看 [项目地图](niuma/项目经理/项目地图.md)，下一项管理交付看 [当前任务板](niuma/项目经理/当前任务板.md)。

最新目录约定：总项目/Git 根目录保留 `./`；自有源码和配套文件统一放到 `./NoManCode`，Rust 工作目录为 `NoManCode/rust-app`；人员档案放 niuma，参考资料放 references。项目名称仍为 NoHumanCode，源码目录使用用户指定的 NoManCode 拼写。源码入口见 [说明](NoManCode/README.md)。

项目正式名称调整为 **NoHumanCode**，`./` 表示整个仓库根。所有人员档案统一进入 [niuma](niuma/README.md)，每人含身份、任务、提交报告、审查与复盘，项目经理同样执行。每轮更新个人身份与交接状态，换对话从 [AGENTS.md](AGENTS.md) 开始。旧品牌与验收事实保留，本机路径已按用户要求相对化，旧哈希对应改写前内容；技术标识保留兼容，当前导航优先于旧快照。

当前工作区是 `./`，已经建立 Git；下文关于旧目录、尚未建立 Git 和早期运行版本的描述保留为历史快照，不再作为开工指令。员工 A 的领域/Repository 基础已通过最终审查，最终修复仍保存在工作区；整个产品尚未完成。

第一轮分工为：B 实现已有无工具 Chat Session 的后续 Turn 应用服务，C 完善现有 HTTP/SSE 传输契约。两者现在均已通过明确范围的最终审查，入口见下；下一阶段接新增 Turn HTTP 写入口，不同时扩大 UI/审批范围。

第一轮历史并行安排（已结束，不再派工）：当时 C 同步实现现有 HTTP/SSE 与独立测试，限定 server 边界，不修改 B 的领域、存储或执行文件。B/C 现已通过；新增 Turn HTTP 写入口以任务板和后续新任务单为准。

首轮历史记录：B 第一轮曾为 `CHANGES_REQUIRED`，涉及回合顺序、前序快照竞争、输入/归属拒绝及有效验收证据；原意见见 [员工 B review](<niuma/员工B/审查记录/第一轮/员工B review.md>)，当前结论以下方最终审查为准。

首轮历史记录：C 第一轮曾为 `CHANGES_REQUIRED`，涉及编码游标、读取故障分类、路径错误、SSE 编帧失败及测试证据；原意见见 [员工 C review](<niuma/员工C/审查记录/第一轮/员工C review.md>)，后续均已闭合。

最新 B 复审：第 1.2 轮 `APPROVED（已有单 Agent、无工具 Chat 连续 Turn 应用服务）`，B-R1～B-R5 全关闭，B-C1 原诊断反例关闭。实际 99 通过、1 忽略，fmt/Clippy/WASM 全通过，另 8+2 审查探针通过。最终契约、证据和非阻断事项见 [员工 B 最终审查](<niuma/员工B/审查记录/第一轮/员工B 最终审查.md>)。B 本轮结束，无需第 1.3 轮。

最新 C 复审：第 1.2 轮已闭合 H7 握手与强制晚等待回归，C-R1～C-R5 全部关闭，结论为 `APPROVED（现有 HTTP/SSE 范围）`。C-R2-01 的新 POST Turn 入口经第2.1轮补证与 [review 2.1](<niuma/员工C/审查记录/第二轮/员工C review 2.1.md>) 复核，已按 NEXT-01 范围验收，待 Git 基线纳入。

开工与协作统一查阅：

- [项目开发守则](项目开发守则.md)
- [第一轮工作总结与员工分工](niuma/项目经理/开发记录/第一轮工作总结与员工分工.md)：三位员工的职责、交付、问题、修法与后续协作方向。
- [核心骨架完成度盘点](niuma/项目经理/开发记录/核心骨架完成度盘点-2026-09-23.md)：按完整 Host 核心范围粗估约四成，明确当前基础、缺口及后续优先级。
- [员工 A 最终审查](<niuma/员工A/审查记录/第一轮/员工A 最终审查.md>)
- [员工 A 复盘与反思日志](niuma/员工A/复盘/员工A复盘与反思日志.md)
- [后续开发注意事项](niuma/项目经理/开发记录/后续开发注意事项.md)
- [员工 B 第一轮任务](niuma/员工B/任务/第一轮/员工B提示词.md)
- [员工 B 最终审查](<niuma/员工B/审查记录/第一轮/员工B 最终审查.md>)
- [员工 C 第一轮任务](niuma/员工C/任务/第一轮/员工C提示词.md)
- [员工 C 最终审查](<niuma/员工C/审查记录/第一轮/员工C 最终审查.md>)

以下原项目书继续约束产品方向；已确认任务单只调整其明确涉及的实施范围。

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

2026-09-23 的现行事实：Git 总根是 ./，源码在 NoManCode/rust-app，代码中的 SCHEMA_VERSION 为 6；A/B/C 第一轮分别通过领域/Repository、无工具连续 Chat 应用服务、现有 HTTP/SSE 的限定验收。C 的新增 Turn HTTP 已按 NEXT-01 范围验收（待 Git 基线纳入）；D 的纯内存插件目录在代码范围已审，完整插件/审批/变更管理/CLI 仍未交付。当前没有据此确认新实例已经运行。

实际状态依据当前代码、[验收索引](niuma/项目经理/关键约定与验收索引.md) 和 [任务板](niuma/项目经理/当前任务板.md) 核对。

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

## 9. 新窗口启动提示

### 给 GPT-6 项目总监

> 接任 NoHumanCode 项目经理。工作区是 ./，请先读取 niuma/项目经理/启动说明.md，按现行地图、任务板、身份和验收证据恢复工作。继续下一项经理交付，Grok 员工负责实现与测试，用户分发提示词；每轮更新身份、任务板和交接记录。

### 给 Grok 4.7 后端实现负责人

> 你负责 NoHumanCode 的 Rust 后端实现。先读取 `./PROJECT-BOOK.md` 和交接文件。优先实现 Plugin Runtime、Context、Registry、依赖解析、可撤销生命周期、Project/Session/Turn 持久化、Approval/Capability、Provider、Workspace、Diff/Checkpoint 和 CLI。每次修改报告 API、Event、Migration、权限影响、失败路径和测试。不要把 UI 临时状态写进领域模型，不要索要或输出 API Key。

### 给 Kimi K3 前端实现负责人

> 你负责 NoHumanCode 的 Leptos/Desktop 前端实现。先读取 `./PROJECT-BOOK.md` 和交接文件。后端协议稳定前可制作 mock client 和协议测试；正式页面必须使用已确认的 API、事件和权限语义。优先实现 Chat、Approval、ToolCall、Diff、Checkpoint、Session 和 SSE 重连。浏览器不得接触 Key、DPAPI、文件、进程或 SQLite。每次修改报告 UI contract、事件消费、失败状态和 E2E 结果。

## 10. 当前下一步

当前C按已冻结的NEXT-01完成新增Turn HTTP并经 review 2.1 复核验收；D-R1-01 / NEXT-02A 与 D-R2-01 / NEXT-02B 均由SWE2max交付并通过经理 review：builtin 插件 Host 注册表、受限 Context、effect 注册与五态可撤销生命周期已随 merge `0fe73f3` 并入 main（不含 wasm/process 运行时、审批与持久化）。后续切片：NEXT-02C ToolCall 逐次审批与 Capability 网关任务包已备待分发（Host 应用层+schema 7 approvals，不含审批传输面）；再接审批 HTTP 入口、wasm/process 插件运行时接入、持久化、工作区变更恢复和 CLI，不能以目录计划、builtin Host 或现有 WASM 宣称完整 Host 完成。前端仍须明确任务，不自行锁定未确认协议。

最初规划的后端切片路径保留作目标参考，其中 Repository、事件和部分执行基础已有验收，其他环节尚未完整交付：

```text
Plugin manifest
→ Host registry
→ Project/Session/Turn repository
→ Event log
→ CLI create/send/status/resume
→ 一个只读工具和一个需审批工具
→ 幂等、取消、失败和重启测试
```

这一切完成后，再将同一协议接入 Desktop。项目持续以小切片、可回退、可审阅、可测试的方式优化，不追求一次性完成所有理想能力。

## 11. 对话与进度记录

项目在 `niuma/项目经理/项目记录/` 中保存两种协作记录：

- `CONVERSATION-FULL.md`：可见的用户消息、助手回复和工具动作元数据；
- `PROJECT-PROGRESS-SUMMARY.md`：项目进度、当前决策、风险、下一步和模型自我述职。

每轮对话结束后同步更新两份记录。全量记录不保存隐藏推理，也不保存未经脱敏的工具输出、API Key 或令牌。新模型或新窗口开始工作时，先读取摘要，再按需读取全量记录和本项目书。
