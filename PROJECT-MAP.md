# Coda CLI 项目地图 (PROJECT-MAP)

> **定位**：本文档是 **Coda CLI** 仓库唯一的现行全局导航、架构拓扑与源码模块索引（Living Project Map）。新协作者或 AI Agent 进入仓库后，请先阅读 [AGENTS.md](AGENTS.md) 了解工程准则，再通过本文档定位模块边界、执行调用链与测试矩阵。

---

## 1. 基线快照与仓库拓扑

### 1.1 当前工程基线（2026-11-19）

| 维度 | 当前事实 |
| :--- | :--- |
| **Git 与 Rust 根目录** | `./`（主分支 `main`，Rust Workspace 已直接提升至仓库根目录） |
| **阶段交付里程碑** | **Stage 1：Windows 本机优先的 CLI AI 编程工作台（Coda CLI）**已完整交付；图形界面（Desktop GUI）明确延后至第二阶段 |
| **包名与二进制标识** | 宿主库与 CLI 二进制名为 **`coda`**（[Cargo.toml](Cargo.toml)），共享协议包为 **`coda-protocol`**（[crates/protocol](crates/protocol)） |
| **数据目录与 Schema** | 默认本机数据目录 `%LOCALAPPDATA%\coda`（可通过 `CODA_DATA_DIR` 或 `--data-dir` 指定），SQLite 数据库 `coda.sqlite3`（WAL 模式，`SCHEMA_VERSION = 19`） |
| **核心环境变量与配置** | `CODA_MODEL`、`CODA_DATA_DIR`、`OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY`。Host 可用 `CODA_KEY_POOLS` 为路由绑定备用凭据。本地 MCP 读取 `.mcp.json` 与 `.coda/mcp.json`，远程 MCP 读取 Host 的 `CODA_MCP_HTTP_SERVERS`。本地 HTTP 鉴权头为 `x-coda-token`。 |
| **Rust 代码规模** | Host 与 CLI 位于 `src/`，共享协议位于 `crates/protocol/src/`，集成测试与夹具位于 `tests/` |
| **质量门禁状态** | [Baseline 6](docs/BASELINE-CHANGELOG.md) 已通过 Windows/MSVC 格式、全目标编译、Clippy 和工作区测试，四项退出码均为 0。 |
| **历史包袱清理与归档** | 旧子目录包装层、空壳 `crates/ui`、死代码 `src/wasm*`、伪插件金字塔与早期多角色文书均已从 `main` 清理；历史快照归档于分支 `archive/gpt-era-20261118`、`archive/old-c35-20261118`、`archive/u18-consolidated-20261118` |

### 1.2 顶层目录结构

```text
./                                  # Git 仓库根 & Rust Workspace 根
├── Cargo.toml                      # Workspace 与主包 coda 定义（Rust Edition 2024）
├── Cargo.lock                      # 锁定的依赖版本树
├── rust-toolchain.toml             # Rust 工具链组件声明（rustfmt / clippy）
├── build.ps1                       # Windows MSVC/SDK 自动探测与构建/检查/测试入口脚本
├── README.md                       # 快速开始、coda CLI 命令速览与核心特性介绍
├── AGENTS.md                       # 核心工程准则与构建/验证命令
├── PROJECT-MAP.md                  # 现行项目地图：架构主链、源码模块全景与测试矩阵（本文档）
├── PROJECT-BOOK.md                 # 长期产品理念、领域边界与架构演进背景
├── CONTRIBUTING.md                 # 协作与提交流程简明指南
├── docs/                           # 公司制研发总控看板（PM-BOARD）、团队领地、ADR 与稳固基线台账
├── crates/
│   └── protocol/                   # 前后端/客户端零 I/O 共享协议类型（coda-protocol）
├── src/                            # Host 核心、CLI 工作台、多模型传输、工具与存储
├── tests/                          # 端到端与子系统集成测试矩阵（13 个测试目标 + support/ 夹具）
├── web/                            # server.rs 内嵌兜底的轻量级静态 HTML/JS 页面（非主开发方向）
└── references/                     # 外部架构与同类 AI 编程工具参考资料
```

---

## 2. 核心架构与两条执行主链

Coda CLI 以单一进程内的 `Store`（SQLite 持久化中心）与 `Engine`（生命周期、工具循环与并发调度核心）为底座，向上提供**两条清晰分离的执行主链**：

```mermaid
flowchart TD
    subgraph Entry["入口与交互层"]
        CLI["链 A：进程内 CLI 工作台<br/>coda init / chat / ask<br/>(src/cli/)"]
        HTTP["链 B：本地 HTTP / SSE Host<br/>coda serve (:3090)<br/>(src/server.rs)"]
    end

    subgraph Core["核心编排与存储层"]
        Engine["Engine 核心调度与工具循环<br/>(src/engine.rs)"]
        Store["Store + Repository (SQLite WAL v19)<br/>(src/store.rs, src/repository.rs)"]
    end

    subgraph LLM["多模型原生流式层"]
        Provider["OpenAI / Anthropic Messages / Gemini<br/>(provider_runtime / provider_transport / provider)"]
    end

    subgraph Tools["受控工具、审批与工作区层"]
        Approval["行级 Myers Diff 预览与 [y/n/a] 审批<br/>(src/approval.rs, src/git_diff.rs)"]
        Workspace["文件读写补丁 / 受控终端 / MCP stdio 与 Streamable HTTP / Worktree<br/>(workspace / command_execution / mcp / git_worktree)"]
        Recovery["原文件前镜像备份与快照恢复<br/>(/diff, /undo, /checkpoint, /restore)"]
    end

    CLI -->|"进程内直连（零 HTTP 仪式）"| Engine
    CLI -->|"会话续接 / 斜杠命令"| Store
    HTTP -->|"同源 Guard / x-coda-token / 幂等键 / SSE 游标"| Engine
    HTTP --> Store
    Engine <--> Store
    Engine --> Provider
    Engine --> Approval
    Approval --> Workspace
    Workspace --> Recovery
    Recovery --> Store
```

### 2.1 链 A：零仪式感进程内 CLI 工作台链（主力日常入口）

面向开发者终端日常高频编码，无需预先启动后台守护进程：

首次配置可用 `coda init --provider openai|anthropic|gemini` 选择厂商预设。在 `coda chat` 中，`/routes` 列出路由，`/route <id>` 切至其他路由时开启新上下文，`/model <name>` 在当前路由内保留会话。`/todos` 显示任务清单，`/checkpoints` 列出当前会话的检查点，尚未选择会话时列出项目内各会话的检查点。

1. **命令入口与会话绑定**：用户运行 `coda init`、`coda chat` 或 `coda ask`（支持零初始化环境变量 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` / `GEMINI_API_KEY` + `CODA_MODEL` 开箱即用，或通过 `-c` / `--continue` / `--resume <id>` 恢复多轮上下文）。
2. **上下文展开与环境感知**：[src/cli/mentions.rs](src/cli/mentions.rs) 解析提问中的 `@file`、`@file:start-end`、`@dir/` 内联引用，拼装行号上下文；自动注入系统环境、Git 仓库状态、项目级 `AGENTS.md` / `CLAUDE.md` 规范，并从 `.mcp.json` / `.coda/mcp.json` 发现注册本地 Stdio MCP 工具（[src/mcp.rs](src/mcp.rs)）。
3. **进程内直连编排**：CLI 在当前进程直接挂载 `Store` 与 `Engine`，省去本地回环 HTTP 序列化开销；长对话支持按原子工具调用组自动或手动压缩（`/compact`）。
4. **多协议原生流式推理**：经 [src/provider_runtime.rs](src/provider_runtime.rs) -> [src/provider_transport.rs](src/provider_transport.rs) -> [src/provider.rs](src/provider.rs) 原生分流 **OpenAI Chat Completions**、**Anthropic Messages（含 Extended Thinking）** 与 **Google Gemini `streamGenerateContent`**，实时流式渲染思维链、正文增量与工具调用，并精确统计每轮 Token 用量（[src/provider_usage.rs](src/provider_usage.rs)）。
5. **行级 Diff 预览与单键审批**：涉及文件修改、终端命令或外部 MCP 调用时，由 [src/approval.rs](src/approval.rs) 与 [src/git_diff.rs](src/git_diff.rs) 在终端即时渲染彩色 Myers Unified Diff，提供单键 `[y/n/a]`（允许本次 / 拒绝 / 本会话自动允许）交互审批（或 `-y` 自动放行）。
6. **受控执行、前镜像备份与随时回滚**：[src/workspace.rs](src/workspace.rs)、[src/command_execution.rs](src/command_execution.rs)（Windows JobObject 子进程树生命周期管控）与 [src/mcp.rs](src/mcp.rs) 执行具体动作。每次写入前自动持久化原文件备份，随时可通过 `/diff`、`/undo`、`/checkpoint`、`/restore` 审阅与精确回滚。

### 2.2 链 B：本地 HTTP / SSE Host 服务链（脚本与未来客户端契约）

面向自动化脚本、外部集成以及未来第二阶段桌面客户端的稳定本地服务边界：

1. **服务启动与单实例锁**：运行 `coda serve`（或无子命令直接执行 `coda --port 3090`），在 [src/main.rs](src/main.rs) 中获取数据目录单实例锁并监听 `127.0.0.1:3090`。
2. **传输层安全与幂等契约**：[src/server.rs](src/server.rs) 实施本机 Host/Origin/Sec-Fetch-Site 同源校验、`x-coda-token` 会话令牌鉴权、`Idempotency-Key` 持久化回执重放以及 SSE 事件流断线游标校验（`Last-Event-ID` / `after`）。
3. **事件溯源与中断恢复**：[src/engine.rs](src/engine.rs) 与 [src/store.rs](src/store.rs) 在 SQLite WAL（`coda.sqlite3`，`SCHEMA_VERSION = 19`）中原子写入领域模型（Project / Session / Turn / Agent / Task）与事件流；若进程异常退出，下次启动自动将未完成任务收敛标记为 `interrupted`，绝不伪造完成状态。

---

## 3. Rust 源码模块全景表

以下完整覆盖 `src/` 下的 源码模块 以及 `crates/protocol` 共享协议包。

### ① 入口与交互层

| 模块文件 | 职责摘要 | 当前状态 |
| :--- | :--- | :--- |
| [src/main.rs](src/main.rs) | 二进制入口、全局 CLI 参数解析、数据目录单实例锁、Tokio 多线程运行时生命周期与优雅退出控制 | 稳定运行 |
| [src/lib.rs](src/lib.rs) | `coda` 库顶层模块导出清单与可见性边界声明 | 稳定运行 |
| [src/cli/mod.rs](src/cli/mod.rs) | CLI 参数、命令分派与共享类型 | 工作台入口 |
| [init_route.rs](src/cli/init_route.rs) | 厂商预设、初始化和路由选择 | CLI |
| [repl.rs](src/cli/repl.rs) | `chat`、`ask`、会话续接、流式渲染与审批 | CLI |
| [mentions.rs](src/cli/mentions.rs) | 展开 `@file`、行号和目录引用 | CLI |
| [http_admin.rs](src/cli/http_admin.rs) | 管理命令的本地 HTTP 客户端 | CLI |
| [src/server.rs](src/server.rs) | Axum 本地 HTTP API 与 SSE 事件流服务、同源与 `x-coda-token` 鉴权 Guard、幂等回执重放及内嵌 `web/` 兜底路由 | 稳定契约 |
| [src/credential_input.rs](src/credential_input.rs) | 安全的终端隐藏回显密钥输入（Windows Console 模式控制）与重定向管道凭据读取校验 | 稳定运行 |

### ② 核心编排与领域存储层（8 个文件）

| 模块文件 | 职责摘要 | 当前状态 |
| :--- | :--- | :--- |
| [src/domain.rs](src/domain.rs) | 核心领域模型定义（`Settings`、`Route`、`Run`、`TaskSpec`、依赖无环图校验、路径范围与输入合法性验证） | 稳定核心 |
| [src/engine.rs](src/engine.rs) | 任务生命周期调度器、全局/单 Key 并发信号量、多轮 Agent 工具调用循环、上下文预算管理、取消与续跑控制 | 稳定核心 |
| [src/engine/exploration.rs](src/engine/exploration.rs) | 单支与并行探索的独立模型上下文、并发调度、父任务的预算及用量结算、按序汇总 | Sprint 05 |
| [src/store.rs](src/store.rs) | SQLite WAL 存储底座（`coda.sqlite3`，`SCHEMA_VERSION = 19`）、平滑 Schema 迁移、事务提交、SSE 事件游标读取、幂等键与崩溃中断恢复 | 稳定核心 |
| [src/exploration_jobs.rs](src/exploration_jobs.rs) | 只读探索批次、子任务与依赖的事务写入，尝试回执关联、崩溃恢复及用量投影 | Sprint 05 |
| [src/repository.rs](src/repository.rs) | 结构化领域仓储层（Project / Session / Turn / Agent / Checkpoint 等实体的持久化读写与状态投影对齐） | 稳定核心 |
| [src/execution_authority.rs](src/execution_authority.rs) | 任务执行授权令牌与状态迁移门禁，确保写操作与工具副作用仅在合法运行周期内落盘 | 稳定核心 |
| [src/checkpoint.rs](src/checkpoint.rs) | 工作区文件快照检查点（Checkpoint）领域结构与恢复元数据定义 | 稳定运行 |
| [src/workspace_changes.rs](src/workspace_changes.rs) | 单轮/跨轮已修改文件追踪记录、变更摘要聚合与 `/undo` 回滚索引辅助 | 稳定运行 |

### ③ 多模型协议、凭据池与预算网络层

| 模块文件 | 职责摘要 | 当前状态 |
| :--- | :--- | :--- |
| [src/provider.rs](src/provider.rs) | 三大协议原生流式编解码器：OpenAI Chat Completions、Anthropic Messages（含 Thinking 块）与 Google Gemini 流式解析及模型列表/额度查询 | 稳定运行 |
| [src/provider_transport.rs](src/provider_transport.rs) | 上游 HTTP 客户端、厂商协议分发、鉴权、流式传输和 Retry-After 解析 | 稳定运行 |
| [src/provider_runtime.rs](src/provider_runtime.rs) | 模型推理运行时封装：将会话历史、System Prompt 与动态工具集组装为对应厂商负载并驱动流式增量回调 | 稳定运行 |
| [src/provider_usage.rs](src/provider_usage.rs) | 流式分块 Token 用量归一化、边界校验与去重累加器（输出 `prompt` / `completion` / `total` 统计） | 稳定运行 |
| [src/budget.rs](src/budget.rs) | 会话与任务级请求轮数上限、Token/费用预算阈值核算、熔断判定与持久审计快照 | 稳定运行 |
| [src/network_authorization.rs](src/network_authorization.rs) | 出站授权、按项目和路由重新检查成员资格、逐次登记网络回执并结算预算 | 稳定运行 |
| [src/key_pool_config.rs](src/key_pool_config.rs)、[src/key_pool.rs](src/key_pool.rs) | 读取 Host 显式绑定到路由及服务地址的备用密钥，按成员轮询及处理 429 冷却 | S3-T3 |
| [src/engine/key_pool.rs](src/engine/key_pool.rs) | 在实际发送前获取所选凭据的并发许可，在同一请求内协调有限次数的尝试 | S3-T3 |
| [src/host_configuration.rs](src/host_configuration.rs) | 宿主机路由、模型配置、默认工作区与账号参数的原子更新校验与快照视图生成 | 稳定运行 |
| [src/secrets.rs](src/secrets.rs) | Windows DPAPI 本机凭据加密/解密（`seal` / `open`）与精确已知密钥日志脱敏（`scrub`，不破坏用户业务代码） | 稳定运行 |

### ④ 编码工具、Git、子进程与 MCP 生态层

| 模块文件 | 职责摘要 | 当前状态 |
| :--- | :--- | :--- |
| [src/workspace.rs](src/workspace.rs) | 内建编码工具箱实现：`read_file`、`edit_file`（精确替换/批量替换/Unified Patch 容错应用）、`write_file`、`glob_files`（`.gitignore` 感知）、`search_files`、`git_status`、`todo_read`/`todo_write`，含路径越界与符号链接防护 | 稳定核心 |
| [src/workspace/exploration.rs](src/workspace/exploration.rs) | Engine 专用工具 `explore_files` 的参数、文件边界和带行号摘录 | Sprint 03 |
| [src/workspace/parallel_exploration.rs](src/workspace/parallel_exploration.rs) | 批量目录校验、按字面检索、合并命中区间及带行号摘录 | Sprint 05 |
| [src/git_diff.rs](src/git_diff.rs) | 纯 Rust Myers 行级 Unified Diff 生成算法、上下文行裁剪与 Git 工作区变更对比 | 稳定运行 |
| [src/approval.rs](src/approval.rs) | 副作用工具执行前的审批状态机、风险分级、待决审批挂起唤醒与会话级自动放行策略 | 稳定运行 |
| [src/tool_runtime.rs](src/tool_runtime.rs) | 工具 Schema 注册表构建、内建工具与动态外部 MCP 工具的统一路由分发器 | 稳定运行 |
| [src/tool_call_application.rs](src/tool_call_application.rs) | 单次工具调用在 Engine 中的完整事务生命周期：参数解析 -> 权限与审批门禁 -> 原文件备份 -> 执行 -> 事件落盘 | 稳定运行 |
| [src/tool_results.rs](src/tool_results.rs) | 工具返回载荷格式化、超大输出安全截断与回传大模型的标准消息结构封装 | 稳定运行 |
| [src/command_execution.rs](src/command_execution.rs) | 受控 `run_command`（PowerShell）执行器：超时控制、标准输出/错误流异步捕获与容量上限裁剪 | 稳定运行 |
| [src/process_lifetime.rs](src/process_lifetime.rs) | Windows JobObject（`KILL_ON_JOB_CLOSE`）子进程树绑定与强杀回收，杜绝僵尸后台进程残留 | 稳定运行 |
| [src/mcp.rs](src/mcp.rs) | stdio 与 HTTP 共用的 MCP 会话池，负责发现、缓存、工具分派与审批目标校验 | Sprint 03 |
| [src/mcp/http.rs](src/mcp/http.rs) | Streamable HTTP 客户端，处理握手、JSON/SSE、工具目录分页、会话标识与关闭 | Sprint 03 |
| [src/mcp/http_registry.rs](src/mcp/http_registry.rs) | 按工作区读取 Host 登记的端点、静态 Bearer 凭据引用或 OAuth 公共客户端配置 | Sprint 04 |
| [src/mcp/oauth.rs](src/mcp/oauth.rs) | 绑定工作区与服务登记。[wire](src/mcp/oauth/wire.rs) 校验元数据并交换令牌，[login](src/mcp/oauth/login.rs) 处理 PKCE 和回环回调，[session](src/mcp/oauth/session.rs) 管理加密凭据、到期与并发刷新 | Sprint 04 |
| [src/git_worktree.rs](src/git_worktree.rs) | 底层 `git worktree` 创建、枚举、状态探查与安全清理操作封装 | 稳定运行 |
| [src/worktree_management.rs](src/worktree_management.rs) | 面向项目与任务的隔离 Git Worktree 生命周期编排、分支绑定与回收管理服务 | 稳定运行 |
| [src/workspace_identity.rs](src/workspace_identity.rs) | Windows 路径规范化、大小写/盘符前缀对齐、真实路径指纹提取与工作区唯一身份识别 | 稳定运行 |
| [src/project_config.rs](src/project_config.rs) | 项目级配置与指令自动加载（发现并注入工作区 `AGENTS.md`、`CLAUDE.md` 及 `.coda/` 项目配置） | 稳定运行 |

### ⑤ 运行时开关与宿主服务层（5 个文件）

| 模块文件 | 职责摘要 | 当前状态 |
| :--- | :--- | :--- |
| [src/agent_runtime.rs](src/agent_runtime.rs) | Agent 角色预设、提示词模板与单任务运行时参数归一化封装 | 稳定运行 |
| [src/plugin_management.rs](src/plugin_management.rs) | 精简后的内建扩展开关持久化状态管理（已剥离旧版多层伪插件目录金字塔） | 轻量保留 |
| [src/runtime_management.rs](src/runtime_management.rs) | 运行时组件启停配置、健康状态查询与宿主机级功能特性开关接口 | 稳定运行 |
| [src/composition.rs](src/composition.rs) | 应用启动装配根（Composition Root）：按依赖顺序组装 `Store`、`Engine`、工具集与宿主服务 | 稳定运行 |
| [src/host_services.rs](src/host_services.rs) | 面向 CLI 管理命令与 HTTP 端点的统一应用服务门面（项目注册、配置更新、Worktree 与状态查询） | 稳定运行 |

### ⑥ 共享协议包（1 个文件）

| 模块文件 | 职责摘要 | 当前状态 |
| :--- | :--- | :--- |
| [crates/protocol/src/lib.rs](crates/protocol/src/lib.rs) | `coda-protocol`：零 I/O 依赖的跨进程/前后端契约定义，包含 `EVENT_SCHEMA_VERSION`、结构化 `ErrorCode` / `ErrorBody`、`SessionKind` 与统一 `LifecycleStatus` 状态枚举 | 稳定契约 |

---

## 4. 自动化测试矩阵地图

集成测试位于 [tests/](tests)，共包含 **13 个 Rust 集成测试目标**。真实 CLI 的端到端用例覆盖[远程 MCP 传输](tests/support/http_mcp_workbench.rs)、[OAuth 登录与刷新](tests/support/oauth_workbench.rs)和[多账号故障转移](tests/support/key_pool_workbench.rs)。

### 4.1 集成测试目标一览

| 测试目标文件 | 覆盖子系统与核心验证点 | 运行方式 |
| :--- | :--- | :--- |
| [tests/cli_workbench.rs](tests/cli_workbench.rs) | CLI 工作台端到端覆盖 `init`、`ask`、零初始化、上下文引用、续聊与并行探索进度，以及远程 MCP 的审批、断流、凭据保护与工作区隔离 | 常规测试套件 |
| [tests/multi_provider_protocol.rs](tests/multi_provider_protocol.rs) | 三种协议的流式事件、脱敏、工具往返与重试；单支与并行探索的上下文隔离、并发上限、预算、撤权与取消回执 | 常规测试套件 |
| [tests/git_diff.rs](tests/git_diff.rs) | **编码工具与 Diff 引擎**：`edit_file`（含多 Hunk Unified Patch 偏移容错）、`glob_files` / `search_files` 分页、`.gitignore` 取反/目录规则与 Myers Diff | 常规测试套件 |
| [tests/subsystems_e2e.rs](tests/subsystems_e2e.rs) | **核心子系统联动**：`edit_file` 审批 -> Checkpoint 创建 -> 精确恢复闭环、会话中断/取消后无缝续聊、MCP 与上下文压缩验证 | 常规测试套件 |
| [tests/tool_call_workflow.rs](tests/tool_call_workflow.rs) | **工具调用与审批工作流**：跨进程 CLI / Host 工具触发、挂起审批、批准/拒绝分支、文件写盘与错误回传 | 常规测试套件 |
| [tests/first_user_workflow.rs](tests/first_user_workflow.rs) | **首日用户全链路体验**：空库首次启动、项目绑定、安全凭据录入、创建会话与完整任务闭环 | 常规测试套件 |
| [tests/host_initialization_workflow.rs](tests/host_initialization_workflow.rs) | **宿主初始化与迁移兼容**：空库安全冷启动、`--legacy-data` 显式导入隔离、权限预算配置与重启状态保持 | 常规测试套件 |
| [tests/host_configuration_http.rs](tests/host_configuration_http.rs) | **配置端点安全与原子性**：Host/Origin/`x-coda-token` 强制鉴权、非法 JSON/越界参数原子拒绝、幂等回执重放 | 常规测试套件 |
| [tests/http_contract.rs](tests/http_contract.rs) | **HTTP / SSE 传输协议契约**：响应安全头、错误码分类、SSE 实时流游标续传、并发 Turn 提交与事务一致性 | 常规测试套件 |
| [tests/runtime.rs](tests/runtime.rs) | **Engine 核心调度与恢复**：多 Key 并发限流、DAG 前置依赖传递、任务取消、崩溃后 `interrupted` 恢复与流式 UTF-8 分块边界 | 常规测试套件 |
| [tests/final_acceptance.rs](tests/final_acceptance.rs) | **领域仓储与投影事务**：Project / Session / Agent / Turn 原子提交、非法依赖或缺失投影时的全量事务回滚 | 常规测试套件 |
| [tests/provider_usage.rs](tests/provider_usage.rs) | **Token 用量核算器**：零值与 SQLite 整数边界、残缺/不一致负载拒绝、碎片化流式快照去重与归一化 | 常规测试套件 |
| [tests/live.rs](tests/live.rs) | **真实上游付费连通性冒烟**（默认 `#[ignore]`）：需显式提供真实测试密钥执行上游模型流式对话与临时文件读写 | `-Action live` 手动触发 |

### 4.2 标准验证命令

在仓库根目录下直接执行：

```powershell
# 1. 代码格式检查
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action fmt

# 2. 全目标静态检查（零警告门禁）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action clippy

# 3. 运行全量单元与集成测试套件
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action test

# 4. 构建 Release 二进制产物（target/release/coda.exe）
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action build

# （可选）单独运行某一个集成测试目标
cargo test --test cli_workbench
```

---

## 5. 当前已完成边界 vs 后续演进方向

### 5.1 已完成并稳定交付（Stage 1 基线）

- **零仪式感 CLI 交互式编码闭环**：`coda init / chat / ask` 进程内直连执行，支持零配置环境变量直连、`-c` / `--resume` 跨调用会话续接、`@file:line-range` 与 `@dir/` 内联上下文引用、自动加载项目 `AGENTS.md` / `CLAUDE.md`。
- **三大主流大模型原生流式直连**：原生支持 OpenAI Chat Completions、Anthropic Messages（含 Extended Thinking）与 Google Gemini `streamGenerateContent` 的流式推理、工具调用与精确 Token 统计。
- **工程级内建工具集与标准 MCP Stdio**：`.gitignore` 感知的高速 `glob_files` / `search_files`、支持批量替换与多 Hunk Unified Patch 的 `edit_file`、Windows JobObject 受控 `run_command`，以及 `.mcp.json` / `.coda/mcp.json` 本地 Stdio MCP 动态工具发现与调用。
- **远程 MCP 工具**支持 Streamable HTTP 的 JSON/SSE 响应、分页发现和工具调用。Host 按工作区登记端点和可选 Bearer 凭据，CLI 审批显示本次调用绑定的地址。[配置示例](README.md#接入远程-mcp-工具)
- **可审阅、可回滚、可恢复的工程韧性**：终端彩色行级 Myers Diff 预览与 `[y/n/a]` 单键审批；文件修改前自动保存前镜像快照，完整支持 `/diff`、`/undo`、`/checkpoint`、`/restore` 与 `/compact`；Git Worktree 隔离管理；SQLite WAL `v18`（`coda.sqlite3`）崩溃安全收敛与 Windows DPAPI 凭据加密。
- **稳固的本地 HTTP / SSE Host 契约**：`coda serve` 提供带同源保护、`x-coda-token` 鉴权、幂等键与游标重连的本地 API，为脚本调用与未来桌面端预留干净的协议边界。

S3-T3 的同路由多账号轮询、429 冷却与故障转移已通过服务器回归和 Windows/MSVC 本机验收。[配置与行为](README.md#为同一路由配置多个账号)

Stage 1.5 的远程 MCP OAuth 已通过 Windows/MSVC 本机验收并登记为 Baseline 5。用户可执行 `coda mcp login <服务名>` 或在聊天内执行 `/mcp login <服务名>`。运行时会刷新过期凭据，授权失效时提示重新登录。[Sprint 04 验证记录](docs/sprints/SPRINT-04-mcp-oauth.md)

[Sprint 03 的只读探索](docs/sprints/SPRINT-03-readonly-exploration.md) 可就指定源码文件生成独立摘要。Stage 1.5 的 `explore_parallel` 支持 1–4 个检索问题，最多两支并发，已通过 Windows/MSVC 验收并登记为 Baseline 6。[S5-T1 验收记录](docs/sprints/SPRINT-05-parallel-exploration.md#服务器验证与-windows-本机验收)

S5-T2 为分支保存独立结果和回执，支持同一会话的后续批次按子任务 ID 使用前批证据，可通过 `coda jobs` 和聊天内 `/jobs` 管理。[S5-T2 实施与验收](docs/sprints/SPRINT-05-parallel-exploration.md#s5-t2)。

### 5.2 暂缓与后续演进方向（Stage 2+ 规划）

- **原生桌面图形界面（Desktop GUI，Stage 2）**：待 CLI 工作流持续打磨成熟后，基于 `coda-protocol` 与本地 HTTP/SSE Host 设计轻量、高响应的桌面工作台界面（可视化多文件 Diff 审阅台、多会话分栏与检查点时间线）。
