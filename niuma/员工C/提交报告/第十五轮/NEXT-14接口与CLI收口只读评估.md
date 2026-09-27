# NEXT-14 接口与 CLI 收口只读评估

更新：2026-09-28；员工C / GPT-6-astra；执行ID C-R15-PRE01-20260928-E01。

## 接收、范围和证据等级

经理明确派发的新前置只读任务，不恢复 C-R14。实际接收主仓 HEAD `97451d6a3c49f618572ecea39c0088f08b496256`、NoManCode tree `5fc44d1d2c4f65790cab94c376c59b1466e88980`、Rust tree `e4a5012dea4d2d0f7acb352de496a66f61bb2097`，初始 `git status --short` 与 `git diff --cached --stat` 输出均空。全部 shell 在经理指定 E 主仓显式 workdir；未访问冻结 D 仓、未读密钥、未启动编译/测试、未操作 Git 写入或创建工作树。仅写本报告和本人身份；Git 模式为主仓文档限定修改、员工无暂存提交权，后续提交及整合人为经理。

已读 AGENTS、本人最新结项、经理R14最终review、niuma README、协作架构、开发/Git/路径规范、PROJECT-BOOK阶段一及进度摘要最新入口。以下均是当前固定产品树静态事实或待经理冻结的建议，不是正式契约、实现授权、测试通过或阶段一验收。

## 当前接口事实

下表行号均相对 `NoManCode/rust-app/`。

| 入口 | 静态事实及影响 |
| --- | --- |
| `src/server.rs:437`、`:1062`、`:1092`、`:1174` | Project config GET/PUT；写入 expected_revision + 完整 config；严格限定 allowed_route_ids/default_route_id/tools/write_scopes/allow_commands 五字段，无预算字段。不可无意用预算更新替换项目权限。 |
| `src/project_config.rs:15`、`:24`、`:43` | ProjectConfig/Snapshot/ConfigureProject 均严格DTO；项目权限配置有独立revision/digest。预算如单独revision，应明确定义和现有config_revision不同。 |
| `src/server.rs:418`、`:428`、`:429`、`:491` | 已有preview、授权确认/撤销、单call、Task calls诊断；network和project读取也须Host token，沿用Host/Origin边界。 |
| `src/network_authorization.rs:84`、`:109` | CallState为claimed/finished/unknown/not_sent；CallReceipt严格字段，无预算预留、用量有效性和结算状态。网络finished不等于预算已结算，更不等于服务商已出账。 |
| `src/server.rs:740`、`:777` | 网络错误为error下 code/message/dispatch_state/calls/retryable(false)，unknown409固定提示查询回执、不自动重试。 |
| `src/cli.rs:2431`、`:2502`、`:2524`、`:2592` | CLI核call唯一ID、归属、方法/来源、状态/时间/结果组合，错误严格DTO及code白名单和HTTP状态。预算错误接入须端到端同时调整，不能只改Server。 |
| `src/cli.rs:2545` | network_request只GET/POST，mutation通信失败为unknown；若预算配置采用PUT，不能原样套用此helper。 |
| `src/server.rs:1284`、`src/cli.rs:1812` | 已有严格唯一幂等header，项目workflow有鉴权读取和PUT分支；可复用安全原则但不依赖路由字符串偶然覆盖。 |
| `src/provider.rs:126`、`:203`、`src/engine.rs:1561` | Provider仅请求/收集usage JSON；Engine把可读u64 token字段累加进Task usage，缺失字段不计。这个聚合不能当成持久、逐call、一次结算的预算账本，缺失不能解释为实际零消耗。 |

## 最小接口建议（全部待冻结）

推荐独立预算资源，避免第一版修改严格的五字段ProjectConfig与旧CallReceipt。预算只覆盖Host经Provider网关的调用，不宣称约束Shell任意网络或服务商账单。经理先与B/D冻结计量单位和保留策略，再定字段：建议首版非货币token预算；不得把max_tokens、账户余额或缺失usage冒充完整成本。

| HTTP建议 | CLI建议 | 必须返回/保证 |
| --- | --- | --- |
| GET `/api/projects/{id}/budget/config` | `project budget config get <id>` | project_id、budget_revision、完整typed config；纯本地查询，不发Provider请求。 |
| PUT `/api/projects/{id}/budget/config` | `project budget config set <id> --expected-revision N --file FILE --key KEY` | 完整config+CAS+单一幂等header；返回不可变receipt与current分开，显式replayed；不改变项目权限或网络grant。 |
| GET `/api/projects/{id}/budget/status` | `project budget status <id>` | budget_revision、计量单位、limit/settled/reserved/available、unresolved_count与blocked_reason；一次一致快照，非CLI自行减法推导。 |
| GET `/api/network/calls/{id}/budget` | `network call-budget <id>` | call_id、project_id、budget_revision_at_claim、reservation、settlement状态和可空usage；不重新网络调用。 |

最小config候选为显式mode和整数limit，具体是否允许unlimited、默认legacy行为、额度耗尽后提高上限、是否允许低于占用的配置等均必须经理冻结。尽量不引入自动周期重置或手工释放unknown入口。若首版只提供CAS、不提供配置幂等，也必须把丢响应后GET核revision/value流程写进契约；不能声称原key可取回原receipt。

严格DTO：所有层deny_unknown_fields；完整字段和Option的显式null策略统一；拒绝未知enum、浮点/负数/溢出、混合单位、空/多值key、跨Project call、重复结算。数量上界应同时满足DB整数和CLI JSON消费约束（若对接JS，冻结安全整数或十进制字符串方案）。status计数与可用值的算术溢出/不一致必须报corrupt；新call预算DTO保持网络状态与结算状态两个独立轴。旧DTO不能静默追加字段，因为严格旧客户端会拒绝。

错误候选：400 budget_invalid；404 budget_not_found；409 budget_revision_conflict / budget_idempotency_conflict / budget_exhausted / budget_unresolved；500 budget_corrupt / budget_internal。保持固定脱敏message、retryable=false与机器code。涉及网络准入失败时沿用dispatch_state/calls；真正发送前拒绝是not_sent、不占额度且Provider计数不增。若前一尝试已发送，必须保留此前call事实，不能统一返回calls=[]。budget错误code须明确扩充现有network error和CLI HTTP状态映射；不要把新预算拒绝降格成generic internal。

幂等建议：配置变更相同scope/key/规范化payload回放同receipt，即使当前revision又变也不伪造新执行；不同payload或scope冲突409。调用预算以稳定call_id为唯一结算键，授权key、Run/Turn key和预算配置key是不同层，不能互换。状态/诊断GET永远无副作用。CLI写入只发一次；断线、超限body、格式错误或不能可信验证响应均显示operation_unknown和可用于查询的ID/key，禁止自动重试、补发Provider请求或换key。

unknown建议：发出后usage缺失、解析不可信、超时、崩溃、持久化失败不能折算成0，也不能按HTTP非2xx推定免费。保留预留额度并显示unresolved；Host重开不得重复结算/重新发送/自动释放。known not_sent方可释放；finished且usage有效后只结算一次。实际usage超出预留上限时应记真实事实并阻断后续，不可截断为上限伪造成本。纯token首版如没有输入token可靠上界，不得声称是计费硬封顶；这一点须和B/D冻结。

## 新用户CLI全链路剩余差距

PROJECT-BOOK:171–224要求单Agent选择项目→Session→对话→读搜→写/命令审批→修改并测试→事件/Diff→接受/恢复/继续，以及默认插件组合、预算与凭据边界。不能用测试总数或“约八成”替代验收。

1. **空数据启动与路由/凭据前置未由现有纯CLI闭环证明。** `src/cli.rs:31`命令表含Route/Account但无Host settings或Key配置入口；`tests/cli_workflow.rs:234`构建真实Host夹具，其中`:252`直接Store写settings/routes，再`:281`开始CLI项目/授权步骤。这证明已配置Host链路，不等于新用户从零只凭CLI完成配置。应先冻结允许的初始化方式：新增安全CLI入口，或明确受支持的Host设置入口及操作说明；不能要求用户直接改SQLite，也不能把明文Key写入示例/命令行参数。
2. **预算和可解释调用诊断尚缺。** 现有network calls能说明发送事实，不能说明预算拒绝/预留/结算/unknown冻结。需新增准入、真实mock Provider计数、并发竞争、有效usage、缺失/畸形usage、超额usage、重启恢复、CAS和丢响应验证。
3. **最新完整组合的新用户验收尚待统一。** `tests/cli_workflow.rs:367`有真实CLI/Host工具恢复新Turn重启，`:561`有killwait，`:625`有事件游标，`:669`有unknown单次；它们是可复用基础，不替代最新NEXT13+预算+独立Shell+文件发现+worktree的一条从空数据操作故事。静态发现不意味着旧测试缺陷或已失败。
4. **默认插件组合需产品命名到实际能力逐项映射。** 项目书列的是产品能力组合，不能仅以plugin list条目数相等判完成；验证禁用后真实注册项/监听/后台任务撤销，并说明尚未覆盖的能力。
5. **文档和可恢复操作需要与同一候选绑定。** 记录安装/启动、版本与端口发现、配置、授权、--json/human、审批网络ack、事件断线、cancel/restart、Diff/Checkpoint/恢复、新Turn、worktree unknown诊断和已知限制。旧fallback可启动、Key不泄漏及五门禁也属于阶段一；付费live未运行须明示，不把mock Provider试用冒充真实付费体验。

建议正式验收最小矩阵：空数据初始化至第一Turn；超额准入无Provider增量；并发只允许额度范围内claim；一次有效usage结算及重复回放；缺失/畸形/超额usage诊断；发送前/发送后/结算持久化点真实killwait重开；预算CAS及丢响应；strict DTO/错误状态/身份交叉校验与无自动重试；日志/JSON/human脱敏；新旧CLI能力主路径和fallback回归。以上均未执行。

## 本轮完成与下一步

完成静态现状、最小API/CLI建议和阶段一剩余差距；没有实现/编译/测试结果。无本人运行中构建。正式NEXT-14契约尚未冻结是实现前置，不是本只读任务阻塞。下一第一操作：交经理审阅计量单位、默认策略、unknown预留、独立DTO兼容和初始化边界，等待正式任务，不自行开工。本报告与身份交付后冻结。
