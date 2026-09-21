# 🍑sh harness 项目迁移交接包

更新时间：2026-09-22  
适用对象：接手本项目的另一个对话、模型或开发代理  
项目名称：🍑sh harness（Peachsh harness）

这份文件是接手入口。先读完本文件，再读取 [总体产品与架构基线](rust-app/MASTER-ARCHITECTURE-BASELINE-2026-09-22.md)。不要把旧评估文档中的“计划中”当成已实现功能。

当前 `D:\peachsh-harness` 不是 Git 仓库，不能假设存在 commit、分支或可用 diff。迁移以磁盘文件和本交接包为准；若需要版本化，应先由新对话建立独立 Git 仓库或备份。

文档优先级：本交接包 → `rust-app/MASTER-ARCHITECTURE-BASELINE-2026-09-22.md` → `rust-app/PHASE0-NEXT-AUDIT.md` → `rust-app/README.md` → 其他历史审计。README 中较早的“Rust 原生对话页已可用”描述不能证明 Leptos 已迁移；实际页面事实以主基线和阶段审计为准。

## 1. 项目位置和运行入口

项目根目录：

```text
D:\peachsh-harness
```

Rust 主项目：

```text
D:\peachsh-harness\rust-app
```

可执行文件：

```text
D:\peachsh-harness\bin\peachsh.exe
```

默认启动脚本：

```text
D:\peachsh-harness\start-peachsh.cmd
```

默认 Rust 服务地址：

```text
http://127.0.0.1:3090/
```

旧 Node 版本仍使用 3080 端口，作为迁移回退，不要把 3080 的页面状态当成 Rust 版本状态。

当前 Rust 实例在本次交接时已经启动。接手前先检查：

```powershell
curl.exe http://127.0.0.1:3090/api/health
```

预期包含：

```json
{"ok":true,"runtime":"rust","schema_version":5,"version":"0.2.0"}
```

如果端口已经被占用，先请求健康接口并检查是否是同一个实例；不要直接再启动一个使用同一数据目录的进程。

启动命令：

```powershell
D:\peachsh-harness\bin\peachsh.exe `
  --data-dir D:\peachsh-harness\data-rust `
  --legacy-data D:\peachsh-harness\data `
  --workspace D:\peachsh-harness\workspace `
  --port 3090
```

检查模式：

```powershell
D:\peachsh-harness\bin\peachsh.exe `
  --data-dir D:\peachsh-harness\data-rust `
  --legacy-data D:\peachsh-harness\data `
  --workspace D:\peachsh-harness\workspace `
  --check
```

## 2. 产品目标

🍑sh 是 Windows 本机单用户 AI 编程工作台，最终要保证完整开发闭环：

```text
选择项目 → 提出目标 → 读取代码 → 获得操作批准
→ 修改/运行测试 → 查看事实和 diff → 接受、恢复或继续
→ 重启后继续同一会话
```

多模型、多 Key 和 Agent Team 是加速能力，不能破坏以下约束：

- Agent 身份、职责、模型和 Key 归属必须稳定；
- 读取自动允许；写入、命令、网络、Git 合并和其他副作用逐次审批；
- shared/worktree 由用户选择，缺少 Git 条件时不能静默降级；
- 任务结果必须可审阅、可恢复、可追溯；
- 余额、token 消耗和金额预算不能混为一谈；
- 浏览器 WASM 不得接触 Key、DPAPI、文件、进程或 SQLite。

第一阶段不做远程多人 SaaS、租户系统、云端任务或跨主机协作。

## 3. 用户已经确认的产品决策

这些决策不要重新询问，除非要改变它们：

1. 产品形态：本机单用户开发工具；
2. 工作区：支持 shared 和 worktree，两种模式由用户选择；
3. 前端：Rust 核心 + WASM 前端；已选择 Leptos CSR；
4. 权限：读取自动允许，写入/命令/网络逐次审批；
5. Team：Lead 先生成计划，用户确认后再派工；
6. 多 Key：健康度选择、限流冷却、失效摘除和任务预算；
7. 目标服务：优先适配 `https://xpeach.codes` 的 OpenAI-compatible API；
8. 产品名称：🍑sh harness。

Leptos CSR 的选择依据见 [框架决策](rust-app/FRAMEWORK-DECISION-2026-09-21.md)，以及 [Leptos CSR 官方文档](https://book.leptos.dev/deployment/csr.html)。Dioxus 仍可作为未来桌面打包方向的备选，但当前不应扩大范围。

## 4. 当前架构

```text
Leptos CSR/WASM UI
        │ HTTP + SSE / peachsh-protocol
        ▼
Rust Host（Axum）
        ├─ Application services
        ├─ Domain state machines
        ├─ Scheduler / task graph
        ├─ Approval / capability policy（尚未完整实现）
        ├─ Provider gateway / KeyPool（KeyPool 尚未完整实现）
        ├─ Project / Workspace / Git（worktree 尚未产品化）
        ├─ Diff / Checkpoint / Delivery（尚未完整实现）
        ├─ SQLite + event log
        ├─ Windows DPAPI credential vault
        └─ Wasmi plugin gateway
```

目标领域对象：

```text
Project
Session(chat | plan | team)
Turn
Agent(stable id, display name, role, model capability)
Task(stable id, dependency by id, status, workspace)
Workspace(shared | worktree | snapshot)
ToolCall / Approval
Artifact / Checkpoint
Provider / ModelProfile / KeyPool / Usage
Event / Idempotency / Migration
```

目标状态机：

```text
draft → awaiting_approval → queued → running
running → completed | failed | cancelled | interrupted
failed/interrupted → awaiting_resume → queued
completed → reviewing → accepted | restored | superseded
```

## 5. 代码事实：已经完成

### 5.1 Host 当前能力

- Axum 本机 HTTP/SSE 服务；
- OpenAI Chat Completions 兼容流式调用；
- 手工多路由、多模型并发；
- 任务依赖图、停止、继续、事件流和 Idempotency-Key；
- 文件读取、搜索、范围写入、文件备份、基础恢复；
- 显式启用的 PowerShell 命令执行；
- Windows DPAPI 加密保存模型 Key 和 New API 账号令牌；
- New API 账号绑定、余额和已用额度查询；
- Wasmi JSON 插件沙箱；
- 旧配置只读导入；
- 单实例锁、启动检查和数据恢复。

### 5.2 协议和迁移

`crates/protocol` 当前提供：

- `SessionKind::{Chat, Plan, Team}`；
- `ErrorCode` 和 `ErrorBody`；
- JSON wire shape 单元测试。

`RunRequest` 显式接收 `kind`。新请求不再通过 `chat-session` 等成员名称判断模式。旧数据库的 `chat-session` 只在 schema 4 → 5 兼容迁移中解析一次，并写入 `schema_migrations`。

不存在的运行组现在返回 HTTP `404` 和 `not_found`；其他尚未分类的失败会保留兼容的 `error` 文本并返回 `request_failed`。

### 5.3 WASM 前端迁移状态

`crates/ui` 是 Leptos 0.8.15 CSR/WASM 迁移壳，已经通过：

```powershell
.\build.ps1 -Action check
.\build.ps1 -Action wasm-check
```

它目前只渲染静态协议状态，没有接入真实项目、会话、SSE 或工具权限。实际可用页面仍是 `web/index.html`、`web/app.js` 和 `web/style.css`，由 Rust Host 内嵌提供。

## 6. 明确未完成的能力

不要把以下内容写成“已经完成”：

- 完整 Leptos 产品页面和浏览器 E2E；
- Project/Session/Turn/Agent/Approval/Checkpoint 的持久化 repository；
- 真正的逐次写入、命令、网络审批界面；
- diff 审阅、接受、恢复、交付和 Git 合并；
- shared/worktree 的产品化隔离和冲突解决；
- 持久 scheduler、后台进程和崩溃后安全恢复；
- Key 健康池、限流冷却、失效摘除、预算和成本归属；
- Lead 计划确认后派工、成员通信、失败重派和团队汇总；
- 上下文压缩、会话搜索/分页/分支/导出；
- MCP、Skills、Hooks、旧 DSH 插件兼容；
- 旧会话的一比一迁移。

当前 `plan` 请求会被明确拒绝：

```text
计划模式尚未进入派工阶段，请先确认计划后再启动团队任务
```

这是未接入“Lead 计划 → 用户确认 → 派工”状态机的明确保护，不是应该通过改成员名称绕过的错误。

## 7. 关键文件地图

```text
D:\peachsh-harness\
├─ HANDOFF-TO-NEXT-MODEL.md                      本文件，唯一接手入口
├─ bin/peachsh.exe                               当前发布 EXE
├─ data-rust/                                    新版 SQLite/配置/加密密钥
├─ data/                                         旧 Node 数据，只读迁移来源
├─ workspace/                                    默认项目工作区
├─ references/                                   参考项目和功能分析
└─ rust-app/                                     Rust workspace
   ├─ MASTER-ARCHITECTURE-BASELINE-2026-09-22.md 唯一主基线
   ├─ PHASE0-NEXT-AUDIT.md                       阶段 0 风险与门禁
   ├─ FRAMEWORK-DECISION-2026-09-21.md           Leptos/Dioxus 决策
   ├─ FRESH-INTEGRATION-AUDIT.md                 代码事实审计
   ├─ Cargo.toml / Cargo.lock                    Rust workspace 与锁文件
   ├─ build.ps1                                  构建、测试、WASM 门禁
   ├─ rust-toolchain.toml                        stable + wasm32 target
   ├─ src/domain.rs                               配置、请求、Task、Run、验证
   ├─ src/engine.rs                               调度、依赖、工具循环、停止/继续
   ├─ src/store.rs                                SQLite、schema 迁移、事件、恢复
   ├─ src/server.rs                               HTTP、SSE、鉴权和错误响应
   ├─ src/provider.rs                             OpenAI-compatible provider 和余额
   ├─ src/workspace.rs                            文件边界和 PowerShell 工具
   ├─ src/wasm.rs                                 插件 ABI 和沙箱
   ├─ src/secrets.rs                              DPAPI 和脱敏
   ├─ crates/protocol/src/lib.rs                  Host/UI wire types
   ├─ crates/ui/src/lib.rs                        Leptos CSR/WASM 迁移壳
   ├─ web/index.html / app.js / style.css         当前实际网页 fallback
   ├─ tests/runtime.rs                            运行时集成测试
   ├─ tests/assessment_adversarial.rs             安全/幂等测试
   ├─ tests/live.rs                               真实 xpeach 测试，默认 ignored
   └─ examples/wasm-echo                          WASM 插件示例
```

参考项目和分析：

```text
D:\peachsh-harness\references\REFERENCE-INDEX.md
D:\peachsh-harness\references\pi-desktop
D:\peachsh-harness\references\dsh-v0.1.6-alpha.2
D:\peachsh-harness\references\codex\FEATURES-AND-DESIGN.md
D:\peachsh-harness\references\claude-code\FEATURES-AND-DESIGN.md
D:\peachsh-harness\references\kimi-code-swarm\FEATURES-AND-DESIGN.md
```

## 8. 构建、测试和发布

在 PowerShell 中执行：

```powershell
Set-Location D:\peachsh-harness\rust-app
.\build.ps1 -Action fmt
.\build.ps1 -Action check
.\build.ps1 -Action test
.\build.ps1 -Action clippy
.\build.ps1 -Action wasm-check
.\build.ps1 -Action build
```

当前已验证：

- 主库测试：13 个通过；
- 安全/幂等测试：4 个通过；
- 运行时测试：9 个通过；
- protocol 测试：2 个通过；
- `fmt`、`check`、`clippy`、`wasm-check` 通过；
- Release EXE 已生成到 `D:\peachsh-harness\bin\peachsh.exe`；
- 真实模型测试 `tests/live.rs` 默认忽略，因为会产生模型调用费用。

真实测试只有在用户明确授权并临时设置环境变量时才运行：

```powershell
$env:PEACHSH_TEST_KEY = '<临时 Key，不要写入文件或提交>'
.\build.ps1 -Action live
Remove-Item Env:PEACHSH_TEST_KEY
```

不要把 Key 写入本文件、日志、数据库、命令脚本或 Git。

## 9. 数据和凭据边界

- 新版数据：`D:\peachsh-harness\data-rust`；
- 旧版数据：`D:\peachsh-harness\data`；
- 项目工作区：`D:\peachsh-harness\workspace`；
- 旧配置导入是只读的；
- 模型 Key 和 New API 令牌使用不同命名空间；
- New API 账号变更地址或用户时必须重新提供令牌；
- 浏览器 API 不返回 Key 明文；
- 备份 SQLite 前先停止实例或使用 SQLite 在线备份，不能只复制正在写入的主库而忽略 WAL。

本文件不包含任何真实 Key。接手模型也不应从旧对话、命令历史或日志中猜测 Key。

## 10. 下一阶段执行任务

下一步只做第一个垂直产品切片：

### Project/Session/Turn + Leptos Chat

目标：把当前聊天流程从旧 HTML/JS 迁移为真正可用的 Leptos 页面，同时保持旧页面可回退。

实施顺序：

1. 在 protocol 中定义 Project、Session、Turn、Message、Event envelope 的最小 wire types；
2. 在 SQLite 中建立只读 Project/Session/Turn 投影，不马上删除旧 `runs/tasks.value`；
3. 增加 Host API：项目列表/选择、Session 列表、创建 Chat、读取消息、SSE 游标重连；
4. 在 `crates/ui` 实现模型选择、消息列表、发送、停止、失败保留草稿和 SSE 重连；
5. 继续由 Host 处理 Key、文件、命令和审批；WASM 不取得宿主权限；
6. 加入浏览器 E2E：打开项目 → 新建 Chat → 收到流式输出 → 断线重连 → 重启后继续；
7. 通过后再迁移审批、diff 和 checkpoint 页面。

验收不能只看“模型返回文本”，必须证明：

- Session 身份与成员显示名无关；
- 重复发送不会重复创建任务；
- SSE 断线后可以从最后游标继续；
- 停止、失败和重启不会丢草稿或伪造完成状态；
- 浏览器永远拿不到任何 Key；
- 旧 HTML/JS 回退仍可启动。

## 11. 给新模型的直接指令

可以把下面这段作为新对话的第一条消息，并附带本文件：

> 你接手的是 `D:\peachsh-harness` 的 🍑sh harness 项目。请先读取 `D:\peachsh-harness\HANDOFF-TO-NEXT-MODEL.md` 和 `D:\peachsh-harness\rust-app\MASTER-ARCHITECTURE-BASELINE-2026-09-22.md`，以它们为唯一当前事实基线。不要索要或输出任何 API Key，不要把旧文档中的计划当成已实现能力。先运行 `curl.exe http://127.0.0.1:3090/api/health`，确认 Rust 服务和 schema 5，再运行 `build.ps1 -Action check`、`test`、`clippy`、`wasm-check`。接着继续实现“Project/Session/Turn + Leptos Chat”第一个垂直切片：先定义 protocol wire types 和只读 SQLite 投影，再接 Host API、SSE 游标重连和 Leptos 页面。保持旧 HTML/JS fallback，不要同时重写 Team、Provider、数据库和所有 UI。每次改动都报告领域对象、API/事件变化、迁移、权限影响、失败路径和验收测试；完成前不要声称产品已经完整迁移。

## 12. 交接完成标准

接手模型在开始改代码前，应能回答：

- 当前实际服务端口是多少？
- schema 当前是多少？
- 哪个页面是实际可用的 UI？
- `SessionKind` 是否由成员名字推断？
- Leptos 页面是否已经接入 Host？
- 哪些操作需要审批？哪些仍未实现？
- 旧数据、Key 和 New API 令牌分别存在哪里？
- 下一阶段唯一垂直切片是什么？

如果这些问题不能从本文件和链接文档中回答，先补充交接文档，不要直接扩大代码改动范围。
