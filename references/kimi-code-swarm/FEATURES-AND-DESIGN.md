# Kimi Code / AgentSwarm：公开功能与 🍑sh 适配分析

核对日期：2026-09-21。本文只整理 Kimi 的公开帮助和文档，不保存 Kimi Code 的产品源码。

## 公开能力

- Agent Swarm 采用 commander（指挥者）加 specialists（专家）结构，由主代理拆分任务、派发并汇总。
- 任务可以并行执行，界面提供任务列表、进度和产物回收；模型池可以让不同成员使用不同模型。
- `AgentSwarm` 工具公开了成员模板、任务项、超时和最大并发等控制面。
- 配置文档把 subagent/swarm 的超时和并发作为可调参数；斜杠命令可打开或关闭 swarm/goal 模式。

## 🍑sh 应吸收

1. 主代理先生成任务图，再按模型能力、上下文长度和预算选择成员。
2. 每个成员都要有稳定的 `team_id`、`member_id`、角色和工作区，禁止用展示名称做身份判断。
3. 任务项必须有状态、依赖、超时、取消、重试次数和结构化产物；成员返回测试证据而非只返回自然语言。
4. 设置并发槽位、单任务预算、全队预算和低余额熔断；失败时允许主代理选择性重派。
5. 前端显示实时进度和成员日志，但把最终合并、提交和恢复留给主代理审批。

## 风险与反驳

- 无限并发会放大成本、限流和上下文冲突，🍑sh 必须默认有限并发。
- “角色名称”不能承担权限和身份语义；这正是当前 Agent Team 命名错乱及前台子代理 `TEAM_NOT_MEMBER` 问题需要回归测试的地方。
- 自动汇总不能替代编译、测试和 diff 审阅。没有证据的“完成”只能标记为待验证。

来源：[Agent Swarm](https://www.kimi.com/en/help/agent/agent-swarm)、[Kimi Code configuration](https://www.kimi.com/code/docs/en/kimi-code-cli/configuration/config-files)、[Kimi Code tools](https://www.kimi.com/code/docs/en/kimi-code-cli/reference/tools.html)、[Slash commands](https://github.com/MoonshotAI/kimi-code/blob/main/docs/en/reference/slash-commands.md)。
