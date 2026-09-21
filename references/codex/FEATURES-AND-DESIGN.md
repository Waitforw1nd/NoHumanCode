# Codex：公开功能与 🍑sh 适配分析

核对日期：2026-09-21。本文只整理公开文档，不包含 Codex 的闭源源码。

## 公开能力

- 代码审阅以差异为中心，结果可以定位到文件和行，便于逐项处理反馈。
- Git worktree 用来隔离任务，多个任务可以并行而不互相覆盖工作目录。
- 子代理适合把检索、实现、测试等边界清楚的工作拆开；主代理负责汇总和最终决策。
- 审批与安全策略把文件、命令、网络和工作目录边界显式化，危险动作可暂停等待用户决定。

## 🍑sh 应吸收

1. 给每个任务生成变更清单、逐文件 diff、接受/撤销和恢复点。
2. 为 Team 成员创建独立 worktree 或目录快照，合并前自动编译和测试。
3. 将工具审批从“总开关”细化为工具、命令模式、目录和网络范围。
4. 让主代理拥有结果汇总权，子代理只提交结构化产物和测试证据。

## 不应照搬

- 不能把公开产品行为当成稳定 API；所有适配都要落到 🍑sh 自己的版本化协议。
- 不保存或逆向闭源实现。🍑sh 的 Rust host、WASM 插件和前端协议必须保持独立。

来源：[Code review](https://learn.chatgpt.com/docs/code-review)、[Git worktrees](https://learn.chatgpt.com/docs/environments/git-worktrees)、[Subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents)、[Approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security)。
