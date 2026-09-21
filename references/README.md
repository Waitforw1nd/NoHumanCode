# 🍑sh 参考项目

这里存放只读参考项目和公开功能分析，与 🍑sh 的 Rust 源码、运行数据和密钥隔离。

## 开源源码快照

### PI-Desktop

- 来源：[vastsa/PI-Desktop](https://github.com/vastsa/PI-Desktop)
- 本地目录：`pi-desktop/`
- 当前快照：`8e3b248`
- 用途：对照持久化 Project/Session/Agent/Work、Rust 主机权限边界、插件贡献类型、会话编排和 E2E 验收设计。

### DeepSeek Harness 0.1.6-alpha.2

- 来源：[deepseek-ai/deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)
- 官方 tag：`dsh-v0.1.6-alpha.2`
- 本地目录：`dsh-v0.1.6-alpha.2/`
- 原始压缩包：`dsh-v0.1.6-alpha.2.full.zip`（SHA-256 `480E7A87A5239E86ECCBDFABDB0B90AB205D0E265DDFDF7070FFC8E66A2219B6`）。由官方 tag 下载并展开，保留完整源码、文档与 MIT LICENSE。
- 用途：对照 DSH 的插件化 host/client、Agent Team、工具协议、持久化会话和测试门禁。它是设计参考，不会作为 🍑sh 的运行时依赖。

## 仅公开资料分析（未保存闭源源码）

以下产品没有在本目录保存源码；只记录公开文档中的可验证功能、架构启发、限制和 🍑sh 的取舍：

- `codex/`：Codex 的代码审阅、工作树、子代理、审批与隔离。
- `claude-code/`：Claude Code 的开发对话、检查点、Agent Teams 和 MCP。
- `kimi-code-swarm/`：Kimi Code 的 AgentSwarm、指挥者/专家模型、并发、超时、进度与产物契约。

详细索引见 [`REFERENCE-INDEX.md`](REFERENCE-INDEX.md)。

参考项目不会被 🍑sh 的启动脚本、构建脚本或运行时自动加载。更新时应重新记录提交号或核对日期，并在 🍑sh 的适配文档中说明吸收的设计和拒绝的设计。
