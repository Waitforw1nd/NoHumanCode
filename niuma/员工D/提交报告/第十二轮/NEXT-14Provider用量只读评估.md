# NEXT-14 Provider用量只读评估

2026-09-28；员工D GPT-6-astra；D-R12-PRE01-20260928-E01。来源为经理本轮明确只读派发及随后Host准入额度方案补充；NEXT-14契约尚未冻结，本报告不是实现授权或验收结论。

## 核收与执行边界

主仓HEAD `97451d6a3c49f618572ecea39c0088f08b496256`；NoManCode tree `5fc44d1d2c4f65790cab94c376c59b1466e88980`；Rust tree `e4a5012dea4d2d0f7acb352de496a66f61bb2097`，逐项实际匹配，开始status/index空。源码目录不是独立Git仓库，以上后二项是HEAD:path树对象，不是对子目录执行rev-parse HEAD所得提交。

已读根AGENTS、README、本人身份及NEXT-13最终review/结项、开发守则、项目书当前说明、经理最新交接、Git/路径/协作规范。所有shell显式活动盘主仓workdir；旧冻结盘未操作。共享只读Git模式，无暂存/提交/merge/建树；Git提交和整合执行人仍为经理。仅写本报告与本人身份。未读取实际凭据、未编译、未执行测试、未发外网或付费调用；下面是固定源码静态证据和建议测试，不是通过记录。

## 结论及经理补充方案

通用OpenAI-compatible路由当前无法证明一次调用真实总token上界，也无法证明美元硬预算。`max_tokens`只是发送给对端的输出参数，输入token、服务端模板/工具开销、不同模型输出与reasoning定义、对端不遵守参数等均不受本机可靠限制；4MB响应限及180秒timeout只限本机接收/等待，不证明远端停止计费。

最小可证明本机硬限是持久claim次数：每次Completion扣一个整数call_unit且unknown/失败不退。但经理最新倾向Project累计total_tokens整数与固定per_call_reservation_tokens（R），明确为Host准入额度，本报告支持该命名与保守语义，不能宣称真实token/美元硬上界。

建议契约精确为：在同一数据库事务核Project预算配置版本、known_total、在途/未知reserved_total与R；checked加法确认 `known_total + reserved_total + R <= limit`，才能同时增加预约与network claim。完成时仅可信完整成功usage结算，释放该R并增加actual；actual>R照实记实际数并置overrun，阻止新Completion，即使当时Project还有余额也不掩盖本次超预约。实际累计>limit当然阻断。所有加法/SQLite整数转换须checked，越界为结构错误阻断，不能wrapping/saturating成可准入值。单调用reservation不是远端使用保证。

unknown、usage missing/invalid、流中断、取消、claim后进程终止、结算持久化失败均保留R并阻该Project新Completion；一个unknown不能只占R后允许继续，因为实际可能无限超过R。已准入的其他并发调用仍可能继续发生消费，预算冻结不能声称撤销它们。配置更新不得清零历史known/unknown、释放旧R或改变旧调用保存的R。手动解锁/对账若不在首版范围则保持只诊断，不能自动释放或重试。

默认unconfigured前向兼容可以维持既有调用，但必须显示未保护；首次配置不得把此前无usage调用表示为零消耗。推荐预算从明确配置revision/epoch生效，历史未覆盖调用独立标记；若存在配置前已claim的在途调用，需冻结启用/阻新调用直至其结果可分类，或明确历史在途不计入本epoch的前向边界，由经理冻结。Models/Balance排除，但route probe中的Completion不能仅因非Task入口而被误称已纳入Project保护：必须决定projectless probe显式不受Project预算、或要求绑定Project。后者涉及范围扩展。

## 源码事实与具体入口

以下路径均相对仓库根，行号来自上述固定基线。

| 文件/行号 | 静态事实与影响 |
| --- | --- |
| `NoManCode/rust-app/src/provider.rs:79–88,126–128` | Decoder.usage为裸Value；每个object直接覆盖前一个。重复冲突会被最后看似有效对象隐藏，非object被忽略。 |
| `provider.rs:117–120,157–186` | DONE要求finish_reason；缺完整结束、length/content_filter、工具参数错误/空内容会失败。存在完整usage也可能因消息失败而失去返回值。 |
| `provider.rs:203–208` | request携max_tokens、stream=true、include_usage=true。请求usage不等于对端必返。 |
| `provider.rs:215–230` | 读流/decoder错误映为Unknown；DONE后停止读取；只有finish成功才返回(message,usage)。DONE后新网络chunk不再等待，不能假定审核了所有尾随数据。 |
| `provider_transport.rs:134–188` | permit不可Clone/Debug，绑定目标/model/user，但不绑定max_tokens或完整body。预算R/配置revision必须持久绑定call_id；不要用现permit匹配冒充预算快照校验。 |
| `provider_transport.rs:228–269` | 15秒连接、180秒总timeout、no_proxy、禁止redirect及retry::never；一次execute，任何execute错误Unknown。call_id未发送到Provider，不是Provider端幂等键。 |
| `network_authorization.rs:419–425,434–437,671–680` | grant绑定目标/model/凭据等，未将max_tokens纳入same_config/digest；预算配置应有独立revision与准入校验，不能指望现network revision随R变化。 |
| `network_authorization.rs:838–918` | Immediate事务复核授权并INSERT network_calls后commit，再造permit。预算预约必须在该事务同生，不能Engine外先扣再claim。 |
| `network_authorization.rs:943–1017` | success、HTTP失败、invalid response、NotSent、Unknown分开；只允许Claimed结算，持久化失败返回PersistenceUnknown。预算结算必须同事务，不得先network Finished再另外更新预算。 |
| `network_authorization.rs:812–825` | 重启把claimed转Unknown/Interrupted；预算须同步保留原预约并冻结相关Project，不能恢复时默认退费。 |
| `domain.rs:37–47,96–99` | Route只有max_tokens等字段；max_tokens校验16–65536并明确输出上限。 |
| `engine.rs:1533–1545` | 限messages序列化1.5MB，在network_admit之前生成tools；该字节限不是输入token上界。 |
| `engine.rs:1558–1577` | 取消作为Unknown；finish_network后才累计Task裸usage，缺字段忽略且u64普通加法。新预算不能依赖该不完整投影；应由持久per-call ledger推导，Task兼容投影也需checked或安全投影。 |
| `engine.rs:318–337` | route probe把max_tokens设128后也调用Completion；不能遗漏非Task付费Completion入口。 |
| `project_config.rs:142–174,284–305` | Project配置的路由允许集合/权限验证存在；预算应按稳定ProjectId归属，不能按route显示名或Task输出汇总。 |

## 保持现complete签名的最小usage设计

无需改变 `complete -> Result<(Value,Value),TransportError>` 或给TransportError加入raw usage。可在Provider侧新增纯 `classify_usage(&Value) -> UsageObservation`，枚举 Missing / Valid(TokenUsage) / Invalid(reason)，Valid仅含prompt_tokens、completion_tokens、total_tokens三个checked整数。公共可见性由经理冻结，可crate可见，供Engine/Store适配调用；不让Provider直接写Store。

1. Null表示Missing；非null非object为Invalid。object必须同时存在三项JSON非负整数并能进入所选存储整数域。字符串数字、负数、浮点、缺字段、溢出均Invalid。`prompt.checked_add(completion)==total`，否则Invalid。未知可选details字段忽略且不落原文，避免reasoning/cache字段再次重复相加；若特定Provider不满足这一约定就不支持其结算而进入invalid，不能猜补。
2. Decoder增加私有 `usage_seen` 与 `usage_invalid`锁存（也可单个内部enum），不再裸object覆盖。usage键不存在或null仅表示该chunk未提供，不清除已见数据；第一次非null先classifier。重复三整数相同接受，不累加；不同或任一invalid永久Invalid，后续合法对象不能洗白。重复可选details差异不影响预算规范值。
3. 保留pub usage Value兼容返回：Missing为Null；Valid输出三项规范整数对象；Invalid输出固定内部标志对象且没有三token字段，如 `{"usage_invalid":true}`。classifier将其判Invalid；不输出原始错误payload。更好的可读实现是内部enum统一在complete末尾转Value，避免在多个分支手动塞sentinel。固定标志仅是内部tuple兼容编码，不是新的外部原文信任来源。
4. Decoder流语义保持；DONE前usage为空仍可完成消息，但预算观察Missing会触发冻结。不要把缺usage直接改TransportError::Unknown而让正文成功/usage缺失失去区分。有效消息可以保留，是否容许同轮工具执行须经理冻结；保守建议阻止继续工具及后续Completion，显式报告需诊断，已经输出的delta是部分观测不等于已结算。
5. 失败usage不必保留给结算：任何complete Err、取消、非2xx或Malformed response都按usage_unknown保留R和冻结。即使decoder此前已看到valid usage，也不据其退预约，因为未得到可信成功终止；代价是更保守，不是不安全。诊断写“结果/用量未知，可能已消耗”，绝不写0。这使原API兼容且避免把错误响应body/原始usage纳入持久层。
6. NotSent只在能证明本机未execute的Invalid分支成立，可原子释放R；不能把HTTP4xx/5xx或连接失败按NotSent退预约。领取后未发送崩溃仍是Unknown，保守持有符合当前claim-before-send语义。
7. 为call_id结算exactly-once：重复相同结果返回既有账目/回执，不再次累加；重复冲突拒绝并保持原账，不覆盖。当前finish_network重复返回Conflict/PersistenceUnknown，新增预算若支持重放必须协调现网络回执语义，不能仅加一个SUM更新。无自动重试；新用户显式调用也须重新准入，且被该Project unknown冻结挡住。

上述sentinel只是最小兼容方案，经理也可冻结独立typed结果结构；后者影响所有complete调用/fixture，应统一声明而不是员工各自改签名。

## 唯一所有权建议

D唯一：`provider.rs`、`provider_transport.rs`及内嵌Provider/transport测试、新独立纯usage模块（如经理批准）。以兼容tuple为优先，transport只在确有预算permit边界变化时修改。B唯一：`network_authorization.rs`、`engine.rs`、Store/schema、domain/config、lib导出及预算事务/恢复/公共DTO；D不因解析需要越权改这些。C唯一：HTTP/CLI/外部协议测试与最终固定组合门禁。现有 `tests/provider_network.rs`/`tests/network_authorization.rs`归属必须在新任务单显式给一个owner，不能默认D/C双写。

先冻结classifier规则及R/limit/epoch/unknown/overrun/NotSent/诊断DTO，再D交纯解析API、B接持久账与调用链、C在固定组合验证。全部是建议；本轮没有创建新实现树或接管源码。

## 建议验收矩阵（本轮全部未执行）

| 场景 | 核心断言 |
| --- | --- |
| 正常usage / tokens=0 / exact R / actual<R | 明确三整数和checked sum；一次claim一次结算；known正确、释放准确R。 |
| 相同重复 / 冲突重复 / invalid后valid / valid后invalid | 相同不相加；其余invalid锁存，不被最后对象洗白。 |
| absent/null / 非object / 缺字段 / string/float/negative / u64或i64边界 | Missing与Invalid可诊断，不能归零/取默认；均冻结并保留R。 |
| total与分项矛盾 / details重复计数 | 不猜补不重复加reasoning/cache；矛盾invalid。 |
| actual>R / actual>remaining / checked加法溢出 | 实际合法可存则照实记超额、置overrun；越存储域保持unknown冻结，不wrap。 |
| usage前后截断、UTF8分块、length/filter、工具JSON坏、emit失败、非2xx、响应超4MB | 不释放R，不把失败看作0；网络receipt与预算状态一致。 |
| connect timeout/流timeout/取消/DONE后连接不闭 | 单次execute，无自动重试；成功DONE不等待连接关闭，失败unknown。 |
| 准入前crash/claim后send前crash/send后结算前crash/结算事务失败 | 真实Host重启；未claim不扣；已claim保留原R且阻Project；network/账目没有半结算。 |
| 同Project多Task/多Host并发/最后一份额度竞争 | SQLite事务只准入可容纳集合，原子claim+R；不靠进程内锁假证明。 |
| 未配置→配置、配置变更与在途旧调用、unknown后尝试新key | 明确前向epoch；旧R不可重算；无法新key绕过冻结。 |
| Project A/B、Models/Balance、route probe Completion | A冻结不误阻B；免费分类不凭路径猜；projectless行为与范围声明一致。 |
| 重复相同结算/冲突结算/恢复重放 | 每call只计一次，SQL账/事件/回执一致，无Provider再次调用。 |
| 失败HTTP及redirect/429/5xx | mock监听器实际请求次数为1；不依据本地错误码推定未消耗。 |

下一第一操作：等待经理冻结上述取舍与正式新任务/基线/唯一文件权；本预审完成待审，无实现授权。历史NEXT-13证据与冻结保持。
## 补充：完整契约v1.0只读复核（2026-09-28）

已完整读取经理 `niuma/项目经理/任务/2026-09-28NEXT-14预算准入与用量诊断契约.md`。本节记录最新已冻结裁决，覆盖上文建议中的未决项：前向coverage；历史unknown明确unmanaged；首配遇既有claimed completion返回busy；成功但missing/invalid可继续合法工具处理，仅阻下一Completion，不改网络成功为失败或撤销审批；预算R与模型max_tokens不耦合；D文件限定provider.rs/provider_usage.rs/tests/provider_usage.rs，不再建议修改provider_transport.rs，lib导出归B。旧建议保留为提出时事实，不构成持续要求。

正式classifier建议（经理/B仍须统一采用）：

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageObservation {
    Missing,
    Valid(TokenUsage),
    Invalid(UsageInvalidReason),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageInvalidReason {
    InvalidShape,
    MissingField,
    InvalidInteger,
    OutOfRange,
    InconsistentTotal,
    ConflictingDuplicate,
}
pub fn classify_usage(value: &serde_json::Value) -> UsageObservation;
```

全字段上界i64::MAX、checked分项之和与total相等。D不要求这些类型Serde派生；B在自己的公共预算DTO转为三整数与固定safe reason。Decoder内部保留Observation锁存，在tuple返回点转换为规范Value；Missing为Null，Valid仅三项，Invalid为固定内部标记。若需保留具体reason，标记只能映固定枚举；其余外部任意字符串不能进入诊断。ConflictingDuplicate经内部标记返回后仍可classifier识别；也可统一InvalidShape但诊断损失具体原因。相同三整数重复接受，null不清除已见值，invalid锁存不可恢复。有效模型消息/合法工具处理与用量分类分离。

完整契约未见必须推翻方案的阻断。实现前应明确B的generic `finish_network<T>`如何获得Completion的typed observation：可额外内部参数或专用finish_completion入口，必须保证network/预算同事务，不得runtime猜T、用Task累计usage倒推或先finish_network后独立settle。typed classifier独立于budget.rs，避免D→B→D循环依赖。JSON Value解析已折叠对象中的重复键；如果经理将“同一个JSON对象重复token键”也纳入严格拒绝，须额外入口级自定义反序列化，不能声称Value classifier本身能够检出；现契约明确要求的重复usage SSE块锁存不受此限制。

本次仍仅只读契约/本人档案增补，无源修改、测试、编译或Git写入。完成后PRE本人报告/身份冻结，主仓index和source由经理实际复核后可登记新基线；正式实现须新执行ID/新工作树与基线真实核收。