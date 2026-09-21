# Claude Code：公开功能与 🍑sh 适配分析

核对日期：2026-09-21。本文只整理 Anthropic 公开文档，不包含 Claude Code 源码。

## 公开能力

- 开发对话可以贯穿读代码、改代码、运行测试和解释结果，而不是只返回一段文本。
- 检查点让用户可以回到会话中较早的文件状态；命令产生的外部副作用仍需单独管理。
- Agent Teams 支持主代理与多个专门成员协作，但官方仍将它作为实验能力，不能假定所有边界都稳定。
- MCP 用统一协议接入外部工具和数据源，工具权限和生命周期需要明确。

## 🍑sh 应吸收

1. 以真实开发闭环作为验收：读项目、精确修改、测试、审阅、恢复、继续对话。
2. 保存可解释的检查点和操作事件，恢复时同时展示无法自动回滚的外部副作用。
3. 将 Agent Team 的成员身份、消息路由、任务状态和失败重派写入持久化协议。
4. 把 MCP/插件能力纳入工具审批、超时、取消和审计。

## 不应照搬

- Agent Teams 的实验状态意味着 🍑sh 需要自己的兼容层和回归测试。
- 不复制闭源实现；对外只保证 🍑sh 自己定义的 Rust/WASM 接口。

来源：[Overview](https://code.claude.com/docs/en/overview)、[Checkpointing](https://code.claude.com/docs/en/checkpointing)、[Agent Teams](https://code.claude.com/docs/en/agent-teams)、[MCP](https://code.claude.com/docs/en/mcp)。
