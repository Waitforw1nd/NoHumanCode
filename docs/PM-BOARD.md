# Coda CLI 项目经理总控看板 (PM-BOARD)

> **跨窗口 30 秒恢复入口**：新会话接手时，请按顺序阅读：`AGENTS.md` -> `docs/PM-BOARD.md`（本文档） -> 当前活跃 `docs/sprints/SPRINT-XX-*.md` -> `PROJECT-MAP.md`。

---

## 1. 当前基线快照 (Current Baseline)

| 指标 | 状态 |
| :--- | :--- |
| **最新基线** | [Baseline 6](BASELINE-CHANGELOG.md)，以 `baseline-1`（`7e9f36d4`）加通过 Windows 验收的 S5-T1 累计补丁定位。 |
| **持久化版本** | Baseline 6 为 SQLite v18；S5-T2 工作区升级至 v19，事件格式仍为 v1。 |
| **当前阶段** | **Stage 1.5：CLI 编程工作台旗舰体验打磨与运行时加固**（Desktop GUI 暂缓至 Stage 2） |
| **当前迭代** | S5-T1 并行检索已验收。下一项 [S5-T2 独立子任务持久化与跨批次编排](sprints/SPRINT-05-parallel-exploration.md#s5-t2) 已实现，等待 Windows/MSVC 本机验收。 |
| **Baseline 6 门禁** | Windows/MSVC 的 `fmt`、`check`、`clippy -D warnings` 和 `test` 均以 0 退出。 |

---

## 2. 最近工单

当前交接见 [S5-T2 验收记录](sprints/SPRINT-05-parallel-exploration.md#验收记录)，上次原生验收见 [S5-T1](sprints/SPRINT-05-parallel-exploration.md#服务器验证与-windows-本机验收)。历轮交付见 [Sprint 04](sprints/SPRINT-04-mcp-oauth.md)、[Sprint 03](sprints/SPRINT-03-readonly-exploration.md)、[Sprint 02](sprints/SPRINT-02-runtime-and-cli.md) 和 [Sprint 01](sprints/SPRINT-01-zero-init-and-repl-ux.md)。

| 工单 | 负责组 | 交付内容 |
| --- | --- | --- |
| S3-T1 | Tech-Runtime、Tech-Provider、Tech-CLI | 主编码会话可以就指定工作区文件发起一次只读探索，返回独立模型上下文生成的摘要。 |
| S3-T2 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | MCP Streamable HTTP 的工作区登记、传输、审批与端到端验证。 |
| S3-T3 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 同一路由的多账号凭据轮询、冷却和有限故障转移。 |
| S4-T1 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 对已登记远程 MCP 服务显式登录，自动刷新过期令牌。 |
| S5-T1 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 主会话内并行检索多个目录，子请求隔离上下文，按输入顺序回传摘要及行号证据。 |
| S5-T2 | Tech-Runtime、Tech-Provider、Tech-CLI、QA-Gate | 子任务独立落盘、按回执恢复，后续批次可引用已保存的结果。 |

## 3. 中长期迭代路线图队列 (Product Backlog)

- **S5-T2 待本机验收**。Windows/MSVC 四项门禁通过后登记下一条基线。[实施顺序与验收场景](sprints/SPRINT-05-parallel-exploration.md#s5-t2)
- **Stage 2 储备：原生桌面审阅工作台 (Desktop GUI)**
  - 待 Stage 1.5 CLI 核心与协议事件全部稳固后，基于 `coda-protocol` 与 `coda serve` 启动多文件 Diff 审阅界面。
