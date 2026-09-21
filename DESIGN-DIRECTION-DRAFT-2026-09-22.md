# 🍑sh harness 设计方向草案

更新时间：2026-09-22  
状态：讨论后的方向记录，不替代 `HANDOFF-TO-NEXT-MODEL.md` 或主架构基线

## 1. 我们要做什么

🍑sh 是 Windows 本机优先的 AI 编程工作台。它最终要让用户完成一条可审阅、可恢复、可继续的开发闭环：

```text
选择项目 → 提出目标 → 读取代码 → 获得操作批准
→ 修改/运行测试 → 查看事实和 diff → 接受、恢复或继续
→ 重启后继续同一会话
```

产品的长期价值不是“调用一个模型”，而是提供一个可以持续组合、替换、审阅和恢复的开发环境。

## 2. 我们借鉴什么，又要超越什么

DeepSeek Harness、Cordis 以及《A Programming Paradigm for Spatiotemporal Composability》提供了重要起点：

- 插件是系统的一等组成部分；
- 组件通过依赖声明连接，而不是依靠隐式全局对象；
- 每个组件的副作用应该可追踪、可撤销；
- 依赖变化应能驱动组件激活、停用和重新加载；
- 应用由核心能力、组合包和配置层叠组成；
- 会话事实、实时状态和用户界面应该分离。

我们不会把 DSH 或 Cordis 当成最终答案，也不会复制它们的实现。它们解决的是动态组合的基础问题，而 🍑sh 还必须解决 AI 编程工作台特有的问题：审批、凭据、模型成本、文件和命令副作用、工作区隔离、Diff、Checkpoint、失败恢复以及 Windows 生命周期。

我们的路线是：吸收原则，验证实现，记录不足，逐步替换和优化。每一次优化都应保持协议、数据、权限和恢复边界清楚，而不是为了追求功能数量扩大不可审阅的内核。

## 3. 核心理念

### 3.1 Host 是上下文、权限和生命周期中心

Rust Host 负责：

- 运行时上下文和插件注册表；
- Project、Session、Turn、Agent、Task 和 Workspace；
- 持久事件、状态投影和迁移；
- Provider、Key、预算和模型调用；
- 文件、Git、Shell 和外部进程；
- 审批、Capability 和安全边界；
- 插件加载、卸载、失败隔离和恢复。

前端、CLI 和插件都不能绕过 Host 直接访问 Key、SQLite、文件系统或进程。

### 3.2 插件是可组合、可撤销、可替换的组件

插件至少要声明：

```text
身份、版本、依赖、提供项、权限、配置、作用域、生命周期和 UI 支持面
```

插件产生的注册、监听器、缓存、子任务和服务都必须能够被 Host 追踪。卸载插件时应撤销这些效果；无法撤销的副作用必须被标记并进入恢复或人工处理流程。

### 3.3 依赖必须显式且可验证

插件通过版本化的 service、command、query、event、resource 和 capability 依赖其他能力。不能通过成员显示名、隐式全局变量或特殊字符串约定建立关键关系。

依赖解析需要逐步支持：

- 命名空间和版本约束；
- 缺失依赖和循环依赖诊断；
- 作用域隔离；
- Provider 替换和滚动切换；
- 本地、WASM 和外部进程实现之间的统一接口。

### 3.4 副作用必须可审阅

读取项目范围内文件默认允许。写入、命令、额外网络、Git 合并、MCP 以及其他外部副作用需要逐次审批。

插件权限声明不等于自动获得权限。权限声明只描述插件可能需要什么；实际调用仍由 Host 的 capability policy 和审批状态决定。

### 3.5 UI 是表现层，不是领域事实

CLI 和 Desktop 使用同一个 Host、同一份事件和同一套权限结果。UI 只发送 typed action、查询 snapshot 和订阅事件。

插件 UI 贡献应先采用声明式模型，例如：

```text
command
panel
form
message block
approval card
diff renderer
status widget
```

插件不得直接修改主页面 DOM、持有领域状态或绕过 Host 执行副作用。

## 4. 两个开发阶段

### 阶段一：CLI 开发者版

CLI 是第一阶段的开发者工作台，也是 Host 和插件系统的参考客户端。它不是另一套产品逻辑。

阶段一先完成单 Agent 的可恢复开发闭环：

```text
选择项目 → 创建 Session → 模型对话 → 读取/搜索代码
→ 请求写入或命令审批 → 修改并测试
→ 查看事件和 Diff → 接受、恢复或继续
```

主要交付：

- 插件 manifest、加载器、生命周期和失败隔离；
- Project/Session/Turn 的可查询持久化投影；
- CLI 命令和稳定的 `--json` 输出；
- Provider、文件工具、Shell、审批、Git Diff、Checkpoint 和恢复插件；
- 事件流、游标、幂等、取消、重启恢复；
- 插件启用、禁用、替换和诊断；
- 默认插件组合包的第一版。

阶段一必须证明：重复请求不会重复副作用，停止和失败不会伪造完成，插件卸载不会留下注册项，Key 不会出现在输出或日志中，旧 HTML/JS fallback 仍可启动。

完整 Team、多 Key 健康池、MCP、Skills、复杂上下文压缩等能力在基础闭环稳定后逐步加入，不在第一阶段同时全部实现。

### 阶段二：Desktop 友好版

Desktop 使用阶段一已经稳定的 Host 和协议，提供更容易理解和操作的图形界面。当前方向仍是 Leptos CSR/WASM；具体桌面外壳在协议和 UI 扩展边界稳定后再决定。

主要交付：

- Project、Session、Chat 和 Turn 页面；
- 流式消息、失败草稿、停止、继续和断线重连；
- 审批卡片、工具调用详情和权限范围；
- 文件树、逐文件/逐行 Diff、Checkpoint 和恢复；
- Team 任务、成员状态和结果汇总；
- 插件面板、消息块、设置卡片和状态扩展；
- 浏览器 E2E、重启继续和凭据不泄漏验证。

Desktop 不重新实现 CLI 阶段的领域逻辑，也不产生另一套数据格式。CLI 继续作为开发者、诊断和恢复入口；是否长期作为公开发行入口，之后根据实际使用决定。

## 5. 默认版与自由度

默认版是一个可替换的插件组合，而不是一个不可拆分的巨型程序。第一版默认组合大致包括：

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

未来可以替换模型、工作区、Shell、文件系统、审批策略、Agent Loop、UI renderer 或持久化提供方。默认插件是我们的产品观点和常用配置，不是唯一实现。

## 6. 逐步优化的工作方法

我们不把当前设计当成终点。每次新增或替换能力时都记录：

1. 它属于哪个领域对象和 capability；
2. 它提供和依赖哪些协议项；
3. 它产生哪些可撤销副作用；
4. 它的状态和事件如何持久化；
5. 它在 CLI、Desktop 和无 UI 模式下如何表现；
6. 它如何处理失败、取消、卸载和重启；
7. 它有哪些权限、成本和数据泄漏风险；
8. 哪些地方仍然只是实验，不应写成已完成。

优化优先级遵循：先保证事实和恢复，再扩大组合自由度；先把接口和失败路径说清，再增加默认功能；先用 CLI 验证真实行为，再把结果交给 Desktop 做友好呈现。

## 7. 当前基线与下一步

当前事实仍以 `HANDOFF-TO-NEXT-MODEL.md` 和 `rust-app/MASTER-ARCHITECTURE-BASELINE-2026-09-22.md` 为准：Rust 服务和 schema 5 已运行，Leptos 仍是迁移壳，旧 HTML/JS 仍是实际页面，许多审批、Diff、Checkpoint、worktree、KeyPool 和 Team 能力尚未完成。

下一步不是立即扩大 UI，而是把这份方向进一步落成三份可审阅设计：

- `PLUGIN-ARCHITECTURE.md`：插件、依赖、生命周期、作用域和权限；
- `CLI-DEVELOPER-EDITION.md`：第一阶段命令、事件、验收和诊断；
- `DEFAULT-PLUGINS.md`：默认插件的边界、组合顺序和可替换点。

这三份设计确定后，再调整 Rust workspace 和代码切片。实现过程中继续保持旧回退入口，避免把“有底层函数”误写成“产品能力已经完成”。
