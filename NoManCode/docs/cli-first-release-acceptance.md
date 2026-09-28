# 第一版 CLI：现状与待验收操作清单

本说明随 NEXT-14 开发，预算能力尚待完整固定候选验收。本文不是阶段一已完成声明；mock Provider、组件测试和付费实机试用分别记录，不能互相替代。程序名仍为 `peachsh`。

## 使用前置与已知缺口

从 `NoManCode/rust-app` 使用项目构建指南生成程序。Host 以 `peachsh --port 3090 --data-dir <数据目录> --workspace <项目目录>` 启动；CLI 子命令使用 `--port` 连接同一 Host，不直接访问 SQLite。数据路径选项只用于启动 Host，不能与 CLI 子命令混用。

当前 CLI 没有完整路由/凭据初始化命令。新用户需要先通过现有 Host 设置入口配置路由和安全凭据，再开始下列操作；这不是“从空数据纯 CLI 完成配置”。不要求用户编辑 SQLite，不把真实密钥放进命令示例、日志或报告。NEXT-14 不新增路由/Key 设置实现。

`--json` 成功输出 JSON 到 stdout；失败说明到 stderr，操作失败 exit 1，参数解析失败可能 exit 2。客户端不会自动重发结果未知的写操作。仅观察命令失败不代表 Host 工作已取消。

## 主路径操作（待统一验收）

| 步骤 | 入口与应观察事实 |
| --- | --- |
| 发现和选择项目 | `project list`、`project create --name <名称> --root <根> --key <唯一键>`、`project config get <项目ID>`；读取当前版本后用完整配置更新，必要时 `project select <项目ID> --expected-revision <版本>`。 |
| 明确网络权限 | `network preview` 查看绑定、版本和purposes，`network authorize` 按该快照显式确认。先用各子命令 `--help` 获取必填参数，不猜测或复制旧授权版本。 |
| 检查路由 | `route models` / `route check` 需要独立网络授权；check中的模型Completion也受预算，Models本身不扣token预算。 |
| 首次对话和继续 | `run start` 创建显式Project中的Run/Session/Turn；`run status`、`run context`、`session status`确认稳定ID，再用 `session send` 创建新Turn。成员显示名不代替身份。 |
| 阅读和发现文件 | 授权的Agent使用文件读/列举/搜索能力；CLI观察事件与结果。写入与Shell需要审批，不把项目权限上限当作单次批准。 |
| 审批与执行 | `task approvals`、`approval get/approve/deny`。Shell网络告知需显式ack；budget不能约束Shell自行发出的网络。 |
| 事件、取消、重启 | `run events` / `session events`保留游标，断线后显式从最后游标继续；`task cancel`后确认终态。重启不能伪造完成、丢草稿或重复副作用。 |
| Diff与恢复 | `task changes/diff/checkpoint`、`checkpoint get/restore`；阅读回执确认真实恢复范围，冲突/unknown不等于成功，不盲目重复写入。恢复后的合法继续使用新Turn，旧封存Task不可resume。 |
| linked worktree | `project worktree create/status/inspect`；新Project独立配置与授权，预算不继承。Git objects/refs/config共享，不是OS隔离。unknown仅诊断，不能自动接纳、清理或解除预约。 |
| 插件生命周期 | `plugin list/status/set-enabled`；验证禁用后能力撤销与运行时收敛，不用命令已接受冒充卸载已完成。 |

上述入口沿用既有命令；示例中的ID、key、版本和目录必须来自当前Host实际查询。完整参数以同一构建的 `--help` 为准。

## NEXT-14 预算操作

以下为冻结契约对应命令，尚待动态验收：

```text
peachsh --port 3090 --json project budget config get --id PROJECT
peachsh --port 3090 --json project budget config set --id PROJECT --expected-revision 0 --limit-tokens 10000 --per-call-reservation-tokens 1000 --key budget-first
peachsh --port 3090 --json project budget status --id PROJECT
peachsh --port 3090 --json project budget call --id PROJECT --call-id CALL
```

先读取预算revision；首次配置revision为0，之后按当前revision CAS更新。原key同参数可显式取回原配置回执，即使当前配置已经变化；不能把旧回执当作当前配置。重用key更改参数会冲突。提高限额或修改单次预约量不清除unknown/overrun，也不改变已有call的预约快照。

预算单位是Provider报告的token，不是费用或真实消耗硬上限。未配置表示预算未启用，历史调用未覆盖；不能解释成免费或消费为零。已配置时限额在每次Completion前检查，一次调用实际消耗可超过预约，必须如实记账并阻止后续调用。Models/Balance不扣此预算，但仍需要网络授权。

`status`即使blocked仍成功exit0并显示阻断原因；spent为已确认消耗，held包含claimed和未知用量预约。usage_missing/usage_invalid/unknown保留预约，不自动解除或重试；overrun已计actual且阻止后续调用。网络完成与预算已知用量是两个维度，必须同时阅读call诊断。无人工settle/release/reset接口。

配置写入断线、响应不能可信校验或stdout输出失败属于操作结果未知；先GET核查或用原key原参数显式重放。不得自动换key再写，不得因CLI错误推断Provider未调用。

## 首版验收登记（尚未执行）

| 事项 | 当前证据边界与待补项 |
| --- | --- |
| 从零上手 | 既有真实CLI/Host工作流测试由Store夹具注入route settings；需完整记录受支持Host配置前置，尚非纯CLI初始化。 |
| 最新组合主路径 | NEXT13已有分项验收；需在含预算的固定候选依上表完成同一新用户故事，覆盖文件发现、独立Shell、Diff/恢复与worktree。 |
| 预算 U01–U10 | 迁移、并发、幂等、有效/异常usage、真实killwait、严格HTTP/CLI与旧行为回归待本轮证据，不预写通过。 |
| 凭据与错误 | JSON/human/日志/事件无密钥；错误分类、unknown单次请求与stdout失败须有证据。 |
| 默认插件组合 | 对照项目书逐项映射能力和真实撤销行为；不以插件数目相等代替验收。 |
| 旧fallback | 仍须实际可启动；未执行不能写通过。 |
| 标准门禁 | 完整候选冻结后唯一串行check/test/fmt/clippy/wasm-check；本说明不构成放行。 |
| 付费真实Provider | 未运行；mock传输和真实本地Host不代表付费全流程试用。 |

正式交付应补入固定源码版本、操作环境、真实退出码、原始日志/发布副本hash与每项结果。阶段一尚未整体完成；Desktop、完整Team、MCP、高级KeyPool不属于本轮。
