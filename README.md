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

当前已验收单 Agent CLI 工具开发链路、真实 Git Diff 与单 Task 最小 Checkpoint。完整第一阶段仍在推进；最新任务、验收范围与限制以[当前任务板](niuma/项目经理/当前任务板.md)和[经理最新进度](niuma/项目经理/项目记录/PROJECT-PROGRESS-SUMMARY.md)为准。

开发和构建方法见 [Rust 源码与构建说明](NoManCode/rust-app/README.md)；项目协作按上述入口继续。
