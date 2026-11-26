# Coda CLI 项目经理总控看板 (PM-BOARD)

> **跨窗口 30 秒恢复入口**：新会话接手时，请按顺序阅读：`AGENTS.md` -> `docs/PM-BOARD.md`（本文档） -> 当前活跃 `docs/sprints/SPRINT-XX-*.md` -> `PROJECT-MAP.md`。

---

## 1. 当前基线快照 (Current Baseline)

| 指标 | 状态 |
| :--- | :--- |
| **最新基线** | [Baseline 8](BASELINE-CHANGELOG.md)，本机验收的 S6-T1 累计补丁已固定为本地集成分支的提交 `1d5ea925`；上一基线为 `b55e04e5`。 |
| **持久化版本** | SQLite v19，事件格式 v1。 |
| **当前阶段** | **Stage 1.5：CLI 编程工作台旗舰体验打磨与运行时加固**（Desktop GUI 暂缓至 Stage 2） |
| **当前迭代** | [S6-T1 Windows 桌面目标观察与事件等待](sprints/SPRINT-06-windows-computer.md#windows-本机验收) 已完成服务器回归、Windows/MSVC 门禁及真实记事本观察验收。保存写入与内容核验留待后续。 |
| **Baseline 8 门禁** | Windows/MSVC 的 `fmt`、`check`、`clippy -D warnings` 和 `test` 均以 0 退出。默认测试 227 通过、0 失败、1 忽略。 |
| **基线后修复（本地集成分支）** | `jobs continue/retry` 与聊天内对应命令已接通 Ctrl+C。含两条真实 Windows 控制台回归，四项门禁退出码为 0，229 通过、0 失败、1 忽略。 |

---

## 2. 最近工单

本轮原生验收见 [S6-T1](sprints/SPRINT-06-windows-computer.md#windows-本机验收)，上一轮见 [S5-T2 验收记录](sprints/SPRINT-05-parallel-exploration.md#验收记录)。历轮交付见 [Sprint 04](sprints/SPRINT-04-mcp-oauth.md)、[Sprint 03](sprints/SPRINT-03-readonly-exploration.md)、[Sprint 02](sprints/SPRINT-02-runtime-and-cli.md) 和 [Sprint 01](sprints/SPRINT-01-zero-init-and-repl-ux.md)。

| 工单 | 负责组 | 交付内容 |
| --- | --- | --- |
| S3-T1 | Tech-Runtime、Tech-Provider、Tech-CLI | 主编码会话可以就指定工作区文件发起一次只读探索，返回独立模型上下文生成的摘要。 |
| S3-T2 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | MCP Streamable HTTP 的工作区登记、传输、审批与端到端验证。 |
| S3-T3 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 同一路由的多账号凭据轮询、冷却和有限故障转移。 |
| S4-T1 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 对已登记远程 MCP 服务显式登录，自动刷新过期令牌。 |
| S5-T1 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 主会话内并行检索多个目录，子请求隔离上下文，按输入顺序回传摘要及行号证据。 |
| S5-T2 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 子任务独立落盘、按回执恢复，后续批次可引用已保存的结果。 |

## 3. 中长期迭代路线图队列 (Product Backlog)

- Windows Computer 后续按控件定向操作、记事本保存核验、图片通道分片推进。[范围和验收设计](sprints/SPRINT-06-windows-computer.md)
- **Stage 2 储备：原生桌面审阅工作台 (Desktop GUI)**
  - 待 Stage 1.5 CLI 核心与协议事件全部稳固后，基于 `coda-protocol` 与 `coda serve` 启动多文件 Diff 审阅界面。
