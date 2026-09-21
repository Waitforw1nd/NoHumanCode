# 🍑sh 参考索引与吸收边界

核对日期：2026-09-21

本目录服务于产品设计、协议兼容和测试用例设计。它不参与 🍑sh 构建，也不应被运行时扫描为插件。开源项目保留官方源码快照；非开源产品只保留公开资料分析，不复制实现细节或受版权保护的源码。

| 项目 | 类型 | 本地内容 | 主要吸收点 | 明确不照搬 |
| --- | --- | --- | --- | --- |
| [PI-Desktop](https://github.com/vastsa/PI-Desktop) | 开源 | `pi-desktop/`，提交 `8e3b248` | Project/Session/Agent/Work、Rust host 权限边界、插件贡献类型、E2E | 不把 renderer 当成可信执行环境；不直接复用其 UI 状态模型 |
| [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) | 开源 | `dsh-v0.1.6-alpha.2/`，tag `dsh-v0.1.6-alpha.2` | 插件化 host/client、工具协议、Agent Team、会话持久化、测试门禁 | 不继续依赖 Node/TS 作为 🍑sh 核心；已知 Team 命名和 `TEAM_NOT_MEMBER` 问题必须有回归测试 |
| [Codex](https://learn.chatgpt.com/docs/overview) | 公开产品资料 | `codex/FEATURES-AND-DESIGN.md` | 变更审阅、隔离 worktree、子代理、审批和安全边界 | 不保存或声称拥有其闭源实现；具体能力以官方文档为准 |
| [Claude Code](https://code.claude.com/docs/en/overview) | 公开产品资料 | `claude-code/FEATURES-AND-DESIGN.md` | 连续开发对话、检查点、Agent Teams、MCP | 不把实验性的 Agent Teams 当作稳定接口；不复制闭源实现 |
| [Kimi Code / AgentSwarm](https://www.kimi.com/en/help/agent/agent-swarm) | 公开产品资料 | `kimi-code-swarm/FEATURES-AND-DESIGN.md` | 指挥者/专家分工、并行槽位、超时、进度、任务列表和产物回收 | 不无限并发；必须有预算、取消、身份和结果契约 |

## 🍑sh 的落地顺序

1. P0：用 Codex/Claude 的工作流要求补齐“对话→读文件→修改→测试→审阅→恢复”。
2. P1：用 Kimi 的 AgentSwarm 形态改造现有 Team，增加自动拆解、模型池、并发上限、失败重派和产物契约。
3. P1：用 DSH/PI 的插件与 host 边界设计统一工具、MCP、权限审批和持久化协议。
4. 每项吸收点都需要在 `rust-app` 中有实现、测试和验收记录；只有文档没有对应测试的能力不能标记为完成。
