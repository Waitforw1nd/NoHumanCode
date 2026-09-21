
# 🍑sh harness 项目进度与模型自我述职摘要

更新时间：2026-09-22
记录规则：每轮对话结束后同步更新；全量对话见 `records/CONVERSATION-FULL.md`。

## 当前项目状态

- 新工作目录：`D:\\code\\fufu`
- 原目录：`D:\\peachsh-harness`，保持不动，作为旧实例和数据来源。
- 新目录已复制源代码、协议、测试、项目书、交接文档、架构基线和参考资料。
- 新目录已排除旧数据、SQLite/WAL、凭据、发布 EXE、`node_modules` 和 Rust `target`。
- 新目录已初始化 Git，当前主分支初始提交为 `4566310`。
- `references/pi-desktop` 保留为本地嵌套参考 checkout，并由主仓库忽略。
- 当前没有在新目录启动实例，也没有复制旧数据到新目录。

## 已确认的产品方向

- 目标是 Windows 本机优先的 AI 编程工作台。
- 插件是可组合、可替换、可撤销的一等组件。
- 借鉴 DeepSeek Harness、Cordis 和时空可组合性，但不止步于它们；逐步验证和优化。
- Rust Host 负责上下文、权限、生命周期、持久化、恢复和宿主能力。
- CLI 是第一阶段的开发者工作台；Desktop 是第二阶段的友好表现层。
- CLI 和 Desktop 共用同一个 Host、协议、事件、数据和默认插件。
- 当前优先后端，不先扩大 Leptos UI。

## 多模型分工

- GPT-6：项目总监，负责范围、架构、契约、验收、决策记录和风险。
- Grok 4.7：后端实现，负责 Rust Host、Plugin Runtime、Store、Protocol、Capability、Provider、Workspace 和 CLI。
- Kimi K3：前端实现，后端协议稳定后负责 Leptos/Desktop、UI slots、SSE 重连和 E2E；可提前制作 mock client，但不自行发明 API。

## 当前最近完成

1. 读取并整理交接文件和主架构基线。
2. 阅读 DSH 架构参考和时空可组合性论文，并形成超越 DSH 的设计方向。
3. 创建方向草案和多模型协作项目书。
4. 创建干净工作目录并建立 Git 基线。
5. 建立本双版本对话记录机制。

## 当前自我述职

已完成：
- 将用户的“全量 + 摘要”要求转为文件化记录机制。
- 尝试读取当前线程的可见历史，用于生成初始全量记录。
- 准备在每轮结束前同步全量记录和摘要。

未完成：
- 本轮最终回复已写入全量记录。
- 后端首个垂直切片尚未开始。

风险与边界：
- 全量记录只保存用户消息、助手可见回复和工具动作元数据，不保存隐藏推理和未经脱敏的原始工具输出。
- 不把任何真实 API Key、令牌或凭据写入记录。
- 旧目录和新目录的数据边界必须保持分离。

## 下一步

1. 由 GPT-6 确认后端优先的契约和任务边界。
2. 由 Grok 4.7 开始 Plugin manifest、Host registry、Project/Session/Turn repository 和事件日志垂直切片。
3. 让 Kimi K3 准备基于已确认契约的 mock client。
4. 每轮同步更新全量记录和本摘要。

## 并行开发提示

在本轮记录提交后，发现 `rust-app` 已出现未提交的后端改动：`crates/protocol`、`src/domain.rs`、`src/lib.rs`、`src/store.rs` 和新增的 `src/repository.rs`。这些改动不是本轮记录机制创建的，已保持原样，没有回滚、覆盖或代为提交。后续由后端负责人先报告变更范围、测试结果和契约影响，再由项目总监决定是否纳入基线。

