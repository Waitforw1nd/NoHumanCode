# 第一版 CLI：现状与待验收操作清单

本说明随 NEXT-15 安全初始化开发。NEXT-14预算切片已有独立验收；NEXT-15真实B/D依赖已整合，C限定编译及I01/I07/I08/I10组件专项已执行通过，最终固定组合五门禁和经理验收仍待放行。本文不是阶段一已完成声明；mock Provider、组件测试和付费实机试用分别记录，不能互相替代。程序名仍为 `peachsh`。

## 使用前置与已知缺口

从 `NoManCode/rust-app` 使用项目构建指南生成程序。Host 以 `peachsh --port 3090 --data-dir <数据目录> --workspace <项目目录>` 启动；CLI 子命令使用 `--port` 连接同一 Host，不直接访问 SQLite。数据路径选项只用于启动 Host，不能与 CLI 子命令混用。

新库不传--legacy-data时只创建空路由、无账户的本地设置，不隐式扫描旧目录或读取旧凭据，不选择Provider或联网。导入旧数据必须首次启动时明确传入--legacy-data；已有Settings的数据库再次启动不会重导入或覆盖。显式导入保留旧兼容行为，不承诺全部旧文件跨文件原子导入。

--check会打开或迁移数据库，必要时写入初始设置；它不是只读诊断。其legacy导入同样需要明确参数。不要把check产生的数据库当作尚未初始化的空库。

`--json` 成功输出 JSON 到 stdout；失败说明到 stderr，操作失败 exit 1，参数解析失败可能 exit 2。客户端不会自动重发结果未知的写操作。仅观察命令失败不代表 Host 工作已取消。

## NEXT-15 安全初始化（组件已验证，最终待验收）

以下命令在连接同一Host的终端执行；替换自己的公开配置。新空库Host revision=0；旧库首次迁移也可能为0，不能据此推断旧配置不存在。先查询再写，不从示例猜测版本。

    peachsh --port 3090 --json host config get
    peachsh --port 3090 --json host route set local --expected-revision 0 --name "Local route" --base-url https://provider.example/v1 --model MODEL --max-tokens 128 --parallel-limit 1 --key setup-first --credential-prompt
    peachsh --port 3090 --json host config operation --key setup-first
    peachsh --port 3090 --json host config get

配置必须且只能选择以下一种模式：

- --credential-prompt：从真实控制台隐藏输入；取消或不能关闭回显时失败，不退化为有回显输入。
- --credential-stdin：从受信任秘密供应方重定向输入一行UTF-8；拒绝TTY、空值、多行、空白/控制字符、非法编码或超过4095字节。仅去掉至多一个末尾LF或CRLF。不用echo或命令行参数拼接真实密钥。
- --keep-credential：保留已有可用凭据；新路由或更换规范化API地址必须replace，不能把旧Key静默转送到新地址。

没有传递秘密值的命令行参数；--key只是本次配置操作的公开幂等key。replace写入当前Windows用户DPAPI存储，清除本路由旧key_env，使环境变量不覆盖新凭据。env/legacy来源继续兼容；credential_source只说明本地来源，不代表Provider已验证。unavailable表示访问失败，不能当成missing并自动覆盖。

新入口仅允许HTTPS或literal loopback IP上的HTTP；localhost/DNS名称、缩写IP不作为HTTP回环目的地。禁止URL userinfo、query、fragment、编码斜线和点段。旧路由不会被静默重写。

保存、回放和查询只处理本地配置：不自动创建/选择Project，不授予工具/写入/Shell或网络权限，不配置/清空预算，不请求Models或Completion。成功回执的revision与route是提交时快照，可能早于当前配置；原回执用operation查询，当前状态用get查询。

PUT交给transport后，断线、超时、重定向、超大或截断响应、非法JSON、身份不一致、不可信响应及stdout失败均为operation_result_unknown、exit1、retryable=false。Host可能已提交，不能从CLI失败推断无写入。先查原key回执，再查当前配置；lookup404仅表示查询快照未见记录，原请求可能仍在途。不会自动重发或换key。需要回放时，用户明确使用原key、原公开参数和原凭据；重新输入不能改变原请求身份。

## 同一新用户最小链（I10）

1. 使用全新数据目录启动Host，确认config get返回空routes；不要预先写SQLite或借旧fixture注入路由。
2. 用安全输入创建一条路由，读取原receipt及当前配置；此时Provider调用、授权及预算调用均为零。
3. 对同一工作目录执行project create，再读project config get。写入完整JSON配置，明确allowed_route_ids/default_route_id；仅本片对话无需工具时保持tools=false、write_scopes=[]、allow_commands=false。使用查询到的配置版本执行config set。
4. 先查project current，再按selection revision显式project select。Project配置版本、选择版本和Host配置revision是不同命名空间。
5. 显式配置Project预算，再network preview；按实际config_revision、credential_revision、binding_digest授予需要的purposes。Completion与Models独立；route check包含Completion并受预算。
6. run start显式指定Project及其配置版本，向本地mock请求一次Completion。核对任务终态、network call和budget call实际usage，不能只看HTTP成功。
7. 保存旧回执后修改显示名，确认原回执仍为旧版本而当前get为新版本；预算、原绑定与调用保持。停止并wait真实Host，重启同一DB并重新bootstrap；再次读回执、当前配置、Project选择、预算和授权，Provider计数不得因重启增加。

I10只证明安全初始化最小链。它不替代默认插件组合逐项装配/撤销，也不代表开发读搜、写入/Shell审批、事件重连、Diff/Checkpoint/冲突恢复与重启新Turn的完整第一阶段验收。

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

以下为NEXT-14已验收切片对应命令；与NEXT-15的最新组合仍需本轮固定证据：

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

## 首版验收登记（组件已执行，完整主路径待验收）

2026-09-28组件受测候选为 `aac520b28eed4f3a60cb9e9d2492bd231b13f96e`，Rust tree为 `63a2b26952faa94b8b60f3bcb0eb92c92a730532`。限定bin check和三个专项文件均exit0：HTTP 3/0/0、CLI 6/0/0、初始化2/0/0，合计11 passed / 0 failed / 0 ignored。这是本地mock及真实Host/CLI组件证据，未执行最终五门禁，也不是APPROVED。首轮I10曾因测试误读网络授权响应封装而失败，修复后重新从组件check串行执行；失败记录及原始/发布证据hash见主仓 `niuma/员工C/提交报告/第十六轮/NEXT-15提交报告0.3.md`。

| 事项 | 当前证据边界与待补项 |
| --- | --- |
| 从零上手 | I01/I10组件通过；同一初始不存在DB、真实binary及CLI完成路由配置、Project权限/选择、预算、Completion授权、一次本地mock Completion及实际usage核查，再kill+wait和同DB重启。Provider计数配置/授权后为0、Completion及重启后为1；原receipt revision=1、当前配置revision=2均保留。没有Store预注入路由。 |
| 最新组合主路径 | NEXT13已有分项验收；需在含预算的固定候选依上表完成同一新用户故事，覆盖文件发现、独立Shell、Diff/恢复与worktree。 |
| 预算 U01–U10 | NEXT-14已有冻结范围验收；NEXT-15组合回归及I10实际usage另记新证据，不把旧结果写成本轮重跑。 |
| 凭据与错误 | I07/I08组件通过，包括严格HTTP、CLI JSON/human输出检查、PUT后断线/不可信响应/stdout失败为unknown且不自动重发。不能以C组件结果替代D输入/crash范围或最终组合验收。 |
| 默认插件组合 | 对照项目书逐项映射能力和真实撤销行为；不以插件数目相等代替验收。 |
| 旧fallback | 仍须实际可启动；未执行不能写通过。 |
| 标准门禁 | 完整候选冻结后唯一串行check/test/fmt/clippy/wasm-check；本说明不构成放行。 |
| 付费真实Provider | 未运行；mock传输和真实本地Host不代表付费全流程试用。 |

正式交付应补入固定源码版本、操作环境、真实退出码、原始日志/发布副本hash与每项结果。阶段一尚未整体完成；Desktop、完整Team、MCP、高级KeyPool不属于本轮。
