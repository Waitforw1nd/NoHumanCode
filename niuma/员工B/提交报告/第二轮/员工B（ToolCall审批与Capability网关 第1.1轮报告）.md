# 员工 B（ToolCall 审批与 Capability 网关 第 1.1 轮报告）

2026-09-25；提交人：员工 B；实际模型 `gpt-5.6-sol`；任务 B-R2-01 / NEXT-02C 修订2；沿用执行标识 `B-R2-01-20260925-0232`。

本轮从待审候选 `50de5456b8c0d412aff4e90821bdde221f066a2d` 接续，提交候选 `7611683b742066f8ac9f553f62c1e40161014c0d`。这是候选交付，不代表经理验收、合并 main 或发布。源码工作树 `../nhc-b-approval`、分支 `codex/b/approval`；提交后干净。第1.0轮由 GPT-6 完成的历史事实不改写。

## 本轮范围与结果

经理有限修订旧测试写权后，适配 `src/store.rs` schema 6→7 最终版本断言，以及 `tests/runtime.rs` 三项旧失败的显式逐次审批。runtime 辅助函数以有界等待只读取指定 task 的唯一 pending 卡，并校验 `tool_name == write_file` 后调用公开 `Engine::decide_approval`；路径拒绝测试的正常写与越界写分别审批。依赖环、硬链接、命令权限、路径拒绝等原断言保留；路径错误断言改为解析工具 JSON 后检查 `error` 字段。

A 独立复核、经理确认并授权两项阻断修复：

- `B-R2-CANCEL-01`：`Engine::cancel` 先通过 Store 验证并持久取消；进程内 active token 存在时再通知，不存在时仍成功。未知 task 继续由 Store 报错。新增真实 runtime 停止/重开，覆盖 pending 与 approved/not_started、重复公开 cancel、resolved 仅一次、批准历史不改写、零执行及 resume 安全取消结果。
- `B-R2-REDACT-01`：`finish_approval` 先把真实工具结果解析为结构化 `Value` 并调用 `redact_persisted`，事件与工具消息均来自同一 safe 值。普通安全结果继续使用紧凑 JSON；若任务层再次文本脱敏会改变紧凑 JSON，则只对已经脱敏的 safe 值使用字符串片段 Unicode 编码，并验证解析等价且二次脱敏稳定。原始秘密不会先编码绕过脱敏。

`Store::finish_approval` 的候选接口由 `Result<()>` 改为 `Result<String>`，返回最终持久化的安全序列化 content；Engine 使用该返回值同步内存消息，避免持久化消息与内存消息分叉。该变化由经理在接口冻结前明确批准，尚未形成传输层冻结契约。

真实 `run_command` 回归输出 Bearer mock、普通 stderr 并退出7：finished 可公开查询，消息解析值与事件 result 相等，数据库不含 mock 秘密；真正停止 runtime 后重开、recover、resume 不重执行，计数仍为1。单元测试另验证普通紧凑输出、结构化错误及中文/emoji 往返。AP16 原完成事务故障/回滚用例随完整 approval_gate 通过，Repository finished 完整性校验未放宽。

## 修改文件

- `NoManCode/rust-app/src/engine.rs`
- `NoManCode/rust-app/src/store.rs`
- `NoManCode/rust-app/tests/approval_gate.rs`
- `NoManCode/rust-app/tests/runtime.rs`

只暂存并提交以上四处。候选字节清单见[候选源码 SHA256](B-R2-01第1.1轮证据/候选源码-sha256.json)。未修改 HTTP、domain、secrets、Cargo、构建脚本或其他员工文件。schema 仍为7，无新增迁移；权限模型未扩展。

## 验证

工作目录均为任务工作树 `NoManCode/rust-app`。PowerShell 7.6.5；在进程设置 `VSINSTALLDIR` 指向本机安装后使用未修改的 `build.ps1` 初始化 MSVC 环境。路径在证据和正文中均用仓库相对表示。

| 命令 | 退出码 / 结果 | 证据 |
| --- | --- | --- |
| `./build.ps1 -Action check; cargo test -p peachsh --locked --lib` | 0；62通过 | [log](B-R2-01第1.1轮证据/lib.final.log) / [JSON](B-R2-01第1.1轮证据/lib.final.json) |
| `./build.ps1 -Action check; cargo test -p peachsh --locked --test runtime` | 0；10通过 | [log](B-R2-01第1.1轮证据/runtime.final.log) / [JSON](B-R2-01第1.1轮证据/runtime.final.json) |
| `./build.ps1 -Action check; cargo test -p peachsh --locked --test approval_gate` | 0；31通过 | [log](B-R2-01第1.1轮证据/approval-gate.final.log) / [JSON](B-R2-01第1.1轮证据/approval-gate.final.json) |
| `./build.ps1 -Action check` | 0 | [log](B-R2-01第1.1轮证据/check.log) / [JSON](B-R2-01第1.1轮证据/check.json) |
| `./build.ps1 -Action fmt` | 0 | [log](B-R2-01第1.1轮证据/fmt.log) / [JSON](B-R2-01第1.1轮证据/fmt.json) |
| `./build.ps1 -Action clippy` | 0 | [log](B-R2-01第1.1轮证据/clippy.log) / [JSON](B-R2-01第1.1轮证据/clippy.json) |
| `git diff --cached --check` | 0；仅四路径白名单 | 提交前实际执行 |

保留的真实失败：首次直接 `cargo test --lib` 仅设置 `VSINSTALLDIR`、未经构建脚本导入 `cl.exe`，退出101，见 [log](B-R2-01第1.1轮证据/lib.initial-no-msvc.log) / [JSON](B-R2-01第1.1轮证据/lib.initial-no-msvc.json)。中途 runtime 发现旧文本搜索不适配安全 JSON、emoji 经 PowerShell 默认管道编码丢失，均修正测试层验证方式后最终通过；对应中间日志保留，未冒充最终结果。

本轮未跑全仓标准 test 或 wasm-check：经理要求先固定 B 候选，随后串行合入 C 的 `891263a4e9628affbf071a7d2ceb8ee71ef137c8` 形成组合 SHA，再由 B 对固定组合执行标准五门禁并新增组合证据。未运行付费 live、发布 build、正式用户库迁移或 OS 断电实验。

## 剩余事项与限制

安全值 Unicode 编码只在紧凑 JSON 经任务层文本脱敏不稳定时启用；这种结果的持久化 content 可读性较低且字节长度增加，但解析后的结构、数字、布尔、null、非 ASCII 文本和错误字段保持不变。此方案仍依赖现有启发式 `redact_persisted` 的识别边界，不声称识别任意未知秘密。

候选仍待经理绑定完整 SHA 审查，并待 C schema7 测试提交串行整合后的标准 check/test/fmt/clippy/wasm-check。下一步第一操作：冻结本提交不再改写，等待经理给出固定组合 SHA 后执行组合五门禁和独立证据。
