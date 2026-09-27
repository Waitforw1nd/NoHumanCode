# NEXT-14预算只读评估

2026-09-28，员工B / GPT-6-astra。任务及执行：B-R13-PRE01-20260928-E01。经理在本轮明确派发的新只读前置任务；旧B-R12已结项，不恢复。本文是待经理取舍的方案，不是冻结契约、产品实现或验收结论。

## 接收与核查边界

只在经理指定的主仓执行，每条shell显式工作目录；冻结的旧盘未访问，未创建临时文件、未读取密钥。实核HEAD `97451d6a3c49f618572ecea39c0088f08b496256`，NoManCode tree `5fc44d1d2c4f65790cab94c376c59b1466e88980`，Rust tree `e4a5012dea4d2d0f7acb352de496a66f61bb2097`。首次及写报告前status/index空。已读根规则、本人最新结项、经理最新交接、B第十二轮最终review及必要源码。只允许本人本报告和身份写入；共享Git index/提交/整合均由经理执行，本任务无Git写权、无独立树。未编译、测试、付费请求或修改产品。

## 同轮经理方案复核：以本节覆盖下方单位建议

经理随后提出Project累计total_tokens整数额度+固定per_call_reservation_tokens（R）。B支持这一更贴近用量的最小方案，优先于下方最初输出token建议；call-count只能保证调用次数，不能代替用量预算。`spent + held + R <= limit` 在network claim同一Immediate事务裁决，所有整数按SQLite范围checked运算。固定R是本地准入占用，不是请求输入token估计或Provider实际费用/用量硬上界；实际usage大于R照实结算，持久overrun并阻止该Project后续Completion，绝不截断。

unknown、成功但missing/invalid usage持久保留R，并阻止该Project新的Completion；这样不允许重复预约继续掩盖未知账。网络finished与预算unknown分离。精确定义有效usage建议要求prompt_tokens/completion_tokens/total_tokens均为非负整数且checked合计一致；如供应商存在不同口径，应明确本版不接受而非猜测。valid actual<=R结算实际total并释放差额，valid actual>R记实与阻塞同事务，NotSent才可释放。已并行claim的其他调用必须允许完成并结算，不能因Project后变blocked而丢掉在途事实。

旧项目未配置明确unmanaged；首次配置前向覆盖，不回填旧Task用量，diagnostic显示覆盖起点及legacy_unaccounted。旧claimed/unknown对应Project仍有未决风险：推荐首次有限配置拒绝，或持久legacy_unknown blocker，两者择一冻结；不能无声当作0。配置CAS必须保持既有spent/held/blocker，调大limit、改R、改为unmanaged均不得自动清除unknown/overrun。若本版只诊断不核销，须明示阻塞不会靠resume或策略更新解除。配置变更只影响之后新claim，账项保存claim时R/policy revision。多个已claim在途结果各自settle，blocked状态由权威账项/持久原因导出，避免一条正常settle把另一条unknown误清除。

本方案不再要求R与route.max_tokens相同，因此下方不可变请求计划是可选的较强完整性建议，不是固定R方案的预算计算前提；route test仍必须纳入同Project，既有max_tokens=128顺序不妨碍固定R计算。其余同事务收尾、独立账项、Task只投影、schema迁移及唯一文件owner建议保持。
## 建议的最小闭环

建议先做“每Project累计Completion输出token额度”，另持久记录prompt/total用量作诊断。预约量取真正发送的 `max_tokens`，并发依据SQLite事务中的已结算额度+所有未决预约总量裁决。它是Host准入限制，不能宣称供应商实际账单/费用硬上限；模型不遵守上限时真实用量仍照录，额度透支阻止后续调用。若经理要求total-token额度，则须另外冻结明确的输入预约算法和不可证明精确上界的限制，不能把字节数估算称为严格token上界。若要求货币额度，还缺模型定价、币种、缓存/推理token口径与修订快照，应另切片。

所有真实Completion都计入所属Project，包括没有Task的route test；Models、account balance和shell任意联网不纳入本切片的token账。新budget sidecar不进入既有ProjectConfig JSON及permission digest，避免预算改动连带作废网络授权/历史Task。配置读写独立revision/CAS；默认迁移为明确未配置/不限制，显示历史未纳账，绝不显示历史花费0。配置额度后仅覆盖新的claim；旧既有未知调用显示legacy_unaccounted，启用有限额度应阻止存在同Project旧claimed/unknown的情况，不能假称额度覆盖旧风险。没有可靠Project归属的旧记录不猜造映射。

## 已核源码接缝（行号对应上述Rust tree）

以下路径均相对于仓库根，行号为只读观测。

| 入口 | 当前事实与下一切片接缝 |
| --- | --- |
| `NoManCode/rust-app/src/network_authorization.rs:630` `checked_authorization` | 最终网络授权/当前绑定复核；预算不能代替授权，不应通过预算配置产生网络grant。 |
| 同文件`:839` `network_admit`；`:846` Immediate；`:882` INSERT claim；`:898` commit | 这是预算预约的最终裁决点。现有preflight仅建议性检查；不得在Engine先读余额后另事务claim。 |
| 同文件`:902` ProviderPermit构造 | commit后发出可发送permit；构造失败经finish走NotSent。预算也应只在确定NotSent时释放。 |
| 同文件`:943` `finish_network`，`:984` Immediate，`:986`重读旧状态 | 现有generic T函数不知道usage；应增加completion专用收尾/显式经校验用量参数，同事务写network结果及账项。 |
| 同文件`:812` `recover_calls_tx` | claimed→unknown；必须同一恢复事务使预算claimed→unknown并保留全部预约，不能自动退款或重发。 |
| `NoManCode/rust-app/src/engine.rs:1541` / `:1559` / `:1562` | 每轮claim→请求→finish之后才累加task.usage；finish成功至Task保存间崩溃会漏投影。因此Task usage不能当账本、不能反向补出历史账。 |
| 同文件`:312` / `:318` route test | 无Task Completion仍有Project subject，且当前claim后才将max_tokens设128；预约必须使用发送前冻结的128，而非原route配置。 |
| `NoManCode/rust-app/src/provider.rs:189` / `:203` / `:224` | complete真实请求包含route.max_tokens，取decoder最终usage；usage可缺失，网络成功不证明用量已知。 |
| `NoManCode/rust-app/src/store.rs:1714` `recover`，`:1724` network恢复 | 与所有旧恢复保持一个事务，不把每项预算恢复拆为独立提交。 |
| `NoManCode/rust-app/src/store.rs:8` / `:296` | 当前schema14；schema15启动及旧fixture精确适配的唯一owner需明确。 |
| `NoManCode/rust-app/src/repository.rs:2193` / `:3192` / `:3219` | network_calls独立表；schema14 apply/verify提供严格DDL与marker模板，但新增15需审前版本校验兼容。 |
| `NoManCode/rust-app/src/project_config.rs:15` / `:217` / `:280` / `:457` | ProjectConfig、config读取、Task绑定复核、configure CAS；预算sidecar可复用Project稳定ID，不改旧JSON/digest。 |

## 持久模型、事务与状态

建议schema15追加 `project_budgets`（project_id FK、revision、可空limit、累计范围说明）与 `budget_calls`（call_id PK/FK network_calls、project_id FK、policy_revision、reserved_output_tokens、state、实际prompt/completion/total各可空、原因及时间）。账项call_id唯一；不以Task/Turn唯一，因为一轮可有多次Completion。三状态最低为reserved/settled/released，再独立unknown或明确unknown状态，防止诊断把未知归作已结算。实际usage与网络状态是两个维度：网络Finished+usage缺失仍是预算unknown。

准入单一Immediate事务：复核授权及Project绑定→读取当前budget policy→校验本次真实请求/预约值正整数且不溢出→按Project汇总settled实际输出与reserved/unknown保留值→checked_add并比较limit→插入network_calls及budget_calls→Task/network事件→commit→唯一permit。任何失败整事务回滚，没有claim/预约/事件残留。不同Store/不同Engine同DB竞争也只允许最后额度一方赢，Engine gate/Semaphore不是最终预算锁。额度更新也在Immediate中与准入串行；降低到当前占用以下建议Conflict而非消除已claim预约。删除Project/策略不得级联丢弃账项。

收尾同一Immediate：重读call与budget旧状态→确认仍由该call收尾→严格解析usage（整数、非负、范围，合计口径冻结，禁止负数/浮点/字符串/溢出转0）→保存network receipt与预算结算/释放/unknown→事件→commit。相同终态重复输入可返回原结算，不重复扣减；不同终态或usage冲突拒绝，绝不覆盖首次事实。汇总以账项为权威，可查询SUM或采用同事务校验的缓存，不另设不可核对的余额。

| 结果 | 网络现有语义 | 建议账项处理 |
| --- | --- | --- |
| 成功且usage完整有效 | finished/success | settled实际值；释放预约与实际差值；超预约照录并阻止之后额度不足请求。 |
| 成功但usage缺失/非法 | finished/success | unknown并保留预约；独立原因usage_missing/invalid，不伪造0。是否连当前Task都失败须冻结，建议保留成功结果并显式显示预算未知。 |
| 已证明未发送（TransportError::Invalid） | not_sent | released；收尾前崩溃仍保守unknown，不用本地推测退款。 |
| HTTP失败/InvalidResponse | finished/failure | 未有权威usage则unknown保留预约；HTTP响应不证明未计费。 |
| 超时/取消/断流/进程中断 | unknown | unknown保留预约，不自动重发或退款。 |
| 持久提交失败 | API PersistenceUnknown | DB旧reserved保留；重启转unknown。禁止后续Task投影成功来补扣或释放。 |

unknown只诊断、不自动重试/人工清账是最小安全范围。用户若需解除应另冻结显式核销审计契约；调高limit是新配置修订，不删除旧unknown。取消Task、resume、新Turn、新Run、切route、重复幂等请求不能释放旧预约或绕过同Project额度。新独立worktree Project预算默认未配置，不继承source额度或账项；产品若要跨Project统一费用上限应另设host/global作用域，不伪称当前Project限额覆盖另一个Project。

## 请求绑定与投影要求

预算占用与claim同事务仍不足以证明实际请求不变。应使用内部不可变CompletionPlan，至少封住project、subject、route/model、max_tokens及请求摘要；permit/派发参数验证同一plan。route test提前把128写入plan，生产工具多轮每一轮独立plan/call账。不把调用者传来的“本次只花1 token”信作预算。即使暂不重构provider，也需在冻结契约中规定Engine唯一路径构造实际route快照且claim后不可扩大上限。

Task.usage可保留为旧兼容投影；新增预算诊断直接由call账项返回已结算/保留/未知及历史覆盖范围。若继续更新Task投影，必须说明它可能包含迁移前历史，不能和新账项SUM等价；禁止用任务投影倒推金额。没有Task的route test在项目账可查，Task列表自然没有该项。NetworkError新增预算typed kind时由传输owner映射，拒绝必须明确NotSent且不返回伪call_id。

## 迁移与兼容

schema15在Immediate中校验14真实DDL/marker→建新表和约束→严格verify→写marker/user_version→commit；并发open、故障marker插入失败必须全回滚。现有schema14验证路径可能对新版本取值做精确判断，升级时逐个复核12/13/14入口，不能只把常量改15。坏15表/index/FK/JSON/行关联拒绝开库，不自修。既有network_calls/Task JSON/events/config/secrets密文逐值保留；新表为空或明确配置默认值，不从task.usage捏造call账，不给旧unknown分配假0余额。新增历史覆盖起点说明应采用迁移时点/账项存在性等可验证条件，而不是声称全部历史都受控。

## 最小验收建议（未执行）

1. 同DB两个真实Store同步竞争最后额度，仅一claim/一次provider发送；进程内锁以外也成立；不同Project独立。
2. claim前、claim提交后发送前、响应后结算提交前、结算后Task保存前真实子进程kill/wait；数据库不出现单边network/budget事实，恢复不重发/退款，Task漏投影不漏账。
3. 多轮工具、route test无Task、resume/新Turn全部逐call预约；route test实际128与预约一致；Models不消耗token额度。
4. 成功有效usage、缺失、负数、浮点、溢出、合计异常、实际大于预约；HTTP失败/断流/取消/Invalid；unknown保留、NotSent释放，重复finish不双扣。
5. policy CAS竞争/降低冲突/额度恰好耗尽/disabled与有限迁移语义；Project权限撤销仍优先正确拦截，授权不能被预算替代。
6. 非空canonical14迁15旧值/rowid/opaque密文保持；并发open、marker失败回滚、坏15拒绝、已有旧unknown启用预算被拦截，legacy不猜造归属。
7. fixed candidate由C唯一五门禁；新增组件与历史fixture适配不得放宽旧快照过滤；全部失败和未执行原样保留。

## 唯一文件所有权建议

这是经理未来任务包建议，不是当前写入授权：B独占新 `src/budget.rs`、`network_authorization.rs`、`engine.rs`、`store.rs`、`repository.rs`、`lib.rs`及必要旧migration/unit fixture；`project_config.rs`原则上零产品改动，若需要登记辅助只由B写。D如承担不可变CompletionPlan与usage规范化，则独占 `provider.rs` / `provider_transport.rs` 和对应新组件测试，B消费真实冻结API，避免两人同时改network_authorization。C独占CLI/HTTP映射、跨接口验收和最终串行门禁；新 `tests/budget_*.rs`按文件再分唯一owner。跨树merge及主仓index只经理操作。第一步先冻结预算单位/作用域/默认值/unknown诊断/策略更新及旧未纳账规则，才能派正式实现。

本轮结论：建议可从现有network claim事务自然接入，但不能仅在Engine比较task.usage。所有方案为静态评估，测试全部未执行。

## 同轮v1.0收口只读复核

经理要求完整只读新契约后复核，本人已完整读取。接受冻结的forward-only：历史unknown明确unmanaged、不纳入新scope；首配与同Project既有claimed completion事务互斥返回busy。因此前文建议旧unknown阻首配已由经理显式取舍覆盖，不再作为阻断。未实现或测试。

必要更正交经理三条：

1. 持久首配coverage边界（同事务可靠高水位/epoch），后续set不更新。验证与诊断区分合法生效前unmanaged和生效后缺账corrupt，避免统一managed:false掩盖损坏；诊断公开覆盖起点。
2. 公共DTO精确冻结：BudgetCall usage为规范三整数typed struct；mode/state/blocked_reason枚举及多blocker优先级；unconfigured的spent/held/count=0、available=null、覆盖起点null。接口owner不自行猜测类型/状态组合。
3. finish同事务严格校验network/budget状态配对；单边终态是corrupt，不独立补写。相同语义回放使用第一次双表持久终态/时间；恢复只同时迁移合法claimed配对。scope外无预算账项调用保持旧network收尾语义。

以上已发经理待决定，本人随后源码/档案/index/no-build冻结，供经理完整基线。当前仅本人两份授权文档已写，其余并行人员/经理档案不触碰；产品和暂存区无本人修改。
