# NoHumanCode

Windows 本机优先的 AI 编程工作台：可组合、可撤销、可替换、可审阅、可恢复。先 Rust Host 与 CLI，再 Desktop。

当前开发根目录用 `./` 表示，即克隆后包含本 README 的目录；克隆位置与目录名可以不同。产品正式名称自 2026-09-23 起为 **NoHumanCode**，员工及项目经理资料统一归入 `niuma/`。文档和脚本遵循 [路径约定](路径与可移植性.md)。

实际项目源码统一位于 [NoManCode](NoManCode/README.md)，Rust 工程在 `NoManCode/rust-app`，配置、测试、构建及启动脚本一并放在源码目录；`references/` 继续存放第三方参考资料。NoManCode 是用户指定的目录名，产品名称仍为 NoHumanCode。

- **换对话先复制：[项目经理启动提示词](项目经理启动提示词.md)**；模型从 [根交接入口](HANDOFF-TO-NEXT-MODEL.md) 开始。
- 员工换对话复制 [员工启动提示词](员工启动提示词.md)；职责、任务状态与接手续读按 [开发协作架构](niuma/开发协作架构.md)。
- [所有角色开工入口](AGENTS.md) 与 [niuma 协作中心](niuma/README.md)
- [项目经理直接启动](niuma/项目经理/启动说明.md) · [项目地图](niuma/项目经理/项目地图.md) · [当前任务板](niuma/项目经理/当前任务板.md)
- [项目书](PROJECT-BOOK.md) 与 [项目开发守则](项目开发守则.md)
- [Git 协作规范](Git协作规范.md) · [参与开发](CONTRIBUTING.md)
- [经理最新进度](niuma/项目经理/项目记录/PROJECT-PROGRESS-SUMMARY.md)
- [当前骨架盘点](niuma/项目经理/开发记录/核心骨架完成度盘点-2026-09-23.md)
- [Rust 源码与构建说明](NoManCode/rust-app/README.md)

第一轮 A/B/C 已按各自范围通过；新 Turn HTTP 写入口、插件 Host、逐次审批、完整工作区变更闭环及 CLI 仍待后续任务。换对话先读取个人身份文件，不能把旧提示词当成当前任务。

本次更新项目名称和协作资料。代码中的 `peachsh` crate、EXE、数据库、协议和配置标识保留兼容，程序展示名称尚待员工实施；本次没有重新构建或替换运行实例。

## 以下为旧版说明与历史快照

下文的旧品牌、./.local/legacy-instance/ 目录、运行能力与缺陷记录属于当时版本。当前状态以上方入口和最新审查为准。

### 原 Rust 0.2.0 说明

最新状态：真实任务与继续流程已验证；本轮发现 6 项待修缺陷，含 2 项凭据隔离问题。请先阅读 [实机评估报告](NoManCode/rust-app/ASSESSMENT-LIVE-2026-09-20.md)。

当前主入口已切换为独立 Rust 应用：双击 `./.local/legacy-instance/start-peachsh.cmd`，访问 http://127.0.0.1:3090/ 。进入“对话”即可直接连续聊天，进入“工作台”可运行 Team/Swarm 任务。可执行程序位于 `bin/peachsh.exe`，运行不需要 Node 或 npm。

支持多 Key / 多模型任务、Team/Swarm 并发与前置依赖、自动汇总、项目工具、停止与继续、New API 余额查询。Key 通过 Windows DPAPI 加密，任务保存到独立的 `data-rust` 目录。

源代码、架构、构建方法与迁移边界见 [Rust 版本说明](NoManCode/rust-app/README.md)；三轮迭代结果见 [迭代计划](NoManCode/rust-app/ITERATION-PLAN.md)，Rust + WASM 取舍见 [架构论证](NoManCode/rust-app/ARCHITECTURE-REBUTTAL.md)，最新全面评估见 [评估报告](NoManCode/rust-app/EVALUATION-2026-09-20.md)。旧会话、插件生态和原版快捷命令尚未迁移，可通过 `start-peachsh-legacy.cmd` 继续使用。下方保留旧版使用说明。

---
# 🍑sh harness

这是基于 DeepSeek Harness `0.1.6-alpha.2` 的本地改造层。原来的 `./.local/legacy-harness/` 保留为回退版本；🍑sh harness 使用独立的 `./.local/legacy-instance/data` 保存会话和配置。

## 启动

双击或在 PowerShell 执行：

```powershell
./.local/legacy-instance/start-peachsh-legacy.cmd
```

启动脚本每次都会先恢复 Team 改造补丁，然后启动 Web 界面。启动端口可以追加，例如 `--port 3092`。

启动前会检查固定的 Node 运行时和 Harness 主程序文件；环境不完整时会直接显示缺失路径，不再只报笼统的“环境准备失败”。

## 配置 Key

先为每个队员路由保存 Key：

```powershell
./.local/legacy-instance/set-peachsh-key.ps1 -Slot 1
./.local/legacy-instance/set-peachsh-key.ps1 -Slot 2
./.local/legacy-instance/set-peachsh-key.ps1 -Slot 3
```

Key 保存到本地凭据存储，不写入 Team 提示词、会话消息或路由模板。每个 Key 对应一个独立的 LLM 路由：`peachsh-key-1`、`peachsh-key-2`、`peachsh-key-3`。

## Team 中的模型绑定

创建队员时，Team 工具新增了这些可选参数：

- `llm_provider`：路由别名，例如 `peachsh-key-2`
- `model`：该路由上的模型 ID
- `reasoning_effort`：模型支持时设置推理档位
- `max_tokens`：本次启动的输出上限

`spawn` / `fork` 仍然只负责选择队员的执行方式，不再被误当作模型路由。独立队员可以同时启动并使用不同 Key。

Team 面板会把成员稳定名称、`teammate` 角色、LLM 路由别名和模型分开显示，避免名称、角色和模型互相覆盖。

## 🍑sh Swarm：不同模型协作开发

参考 Kimi Code Swarm 的“模板 + 独立工作项 + 统一汇总”模式，🍑sh harness 增加了三个 Team 工具：

- `swarm_start`：一次提交多个独立工作项，每个工作项可以指定不同的 `llm_provider`、`model`、推理档位和输出上限；启动并发受上限控制，默认最多 3 个。
- `swarm_wait`：按 `run_id` 等待这一批队员收敛，并返回每个成员的状态。
- `swarm_resume`：向指定成员发送持久化跟进消息；成员休眠时会冷恢复，并继续使用原来的模型路由。

每个工作项都必须有稳定的 lower-kebab-case 名称、职责和任务；可以声明 `write_scopes`，队员提示词会要求只修改自己的范围并向 Lead 汇报文件、测试、风险和下一步。不同模型的差异保存在队员的持久路由配置中，后续消息不会回退到 Lead 模型。

同一批次的写入范围如果相同或互相包含，`swarm_start` 会直接拒绝这批任务；需要共享同一目录时，应先拆成有依赖关系的 Team task，按顺序执行。

典型分工是：强推理模型做方案和疑难调试，编码模型做实现，轻量模型做测试、审查和文档。共享工作区仍需拆分不重叠的写入范围；有依赖的工作使用现有 Team task board 排序，不把依赖任务伪装成并行任务。

## 接入地址

当前模板使用 `https://xpeach.codes/v1` 作为 OpenAI 兼容 API 根地址，默认示例模型为 `gpt-5.6-sol`。若账户实际分配了其他 API 根地址或模型，只需修改 `data/settings.yaml` 中三个 `baseURL` / 模型 ID，不要把 Key 写进去。

## 绑定 New API 账号并查余额

🍑sh harness 现在提供一个本地 `newapi` skill。它使用 New API 的 `/api/user/self` 接口读取账号资料和余额，令牌只写入本地凭据存储，不会进入 Team 提示词、会话消息或查询结果。

首次绑定时在 PowerShell 执行：

```powershell
./.local/legacy-instance/newapi.cmd bind
```

按提示填写 New API 的根地址、用户 ID 和访问令牌。之后查询：

```powershell
./.local/legacy-instance/newapi.cmd balance
```

在 Web 对话中也可以直接输入 `/newapi balance`。余额和已用额度会按 New API 的原始 quota 单位除以 500000 显示；HTTP 错误只显示状态和脱敏摘要。

## 使用 Swarm 开发

在 Web 对话中输入 `/swarm`，或直接说明“用 Swarm 并行开发这个功能”。🍑sh harness 会按工作项启动不同模型的队员，默认最多同时接纳 3 个；每个队员保留自己的 Key、模型和推理设置，完成后由 Lead 统一审查和合并。

