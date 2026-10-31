# NEXT-16 内核 Provider 服务与完整 CLI 链路

日期：2026-10-31。项目经理制定。状态：v1.0正式冻结并派发。基线与唯一写入范围见末节执行登记；本契约授权三名现有6.1 Terra员工实施NEXT-16，不授权修改其它范围。

## 用户目标与本轮作用

本轮承接用户“先继续内核、三个6.1 Terra员工、直到整个项目完成”的目标。项目书默认 provider-xpeach 能力当前由 Engine 直接调用 provider::complete；文件与Shell已有实际插件服务绑定。建立真实的Completion协议服务绑定并用从空数据开始的开发闭环验收，作为完整默认组合和后续Desktop的基础。不把本轮替代完整项目，不凭注册字符串宣称其余默认插件已完成。

## 不变量与实际范围

- 所有生产 Completion 入口均经过同一受控服务，包括 Agent 每轮和 route check 的 Completion 部分；Models/Balance保留原路径，不能留下未说明的Completion直连回退。
- PluginHost负责精确版本、显式依赖、服务实例与effect；ProviderRuntime负责准入和不可复制lease。受信内置codec只获得模型、messages、tools和流数据，不获得Store/Engine/ProviderClient/ProviderPermit/凭据。
- Host独占网络目标、认证、模型与用途核对、发送、network/预算claim和finish、unknown及重启恢复。插件不决定调用的权限或持久完成事实。
- 现有请求、SSE分块/工具参数、usage规则、响应上限、错误分类、回放与CLI行为保持；新增前置服务不可用必须明确未发送，不制造网络/预算claim。已claim后的失败必须交原finish路径，不丢失持久事实、不自动重发。
- 启动绑定失败fail closed。禁用关新准入，已经成功准入的lease可以完成，effect撤销不冒称外部网络效果撤销。不得持锁await，不在Store锁内取runtime锁，既有Engine gate→runtime→短Store调用顺序需被明确审计。
- 这是受信任进程内Rust组件边界，不是恶意插件沙箱。WASM/process、任意第三方加载和全部13项默认装配仍有后续任务。
- 无新增schema、依赖或旧数据重写的预设需求；若实现发现必要变化，给出具体理由并由经理修订。

## 接口方向

建议单独provider_runtime模块定义版本化Completion codec与decoder。输入使用不含base_url/key_env或秘密的最小DTO；codec构造请求及增量解码，Host校验模型、stream、max_tokens、工具集合与整体大小，不允许codec改变出站目标或用途。codec错误和panic使用固定安全错误，不将prompt/响应或自定义panic内容写日志。普通内置实现复用既有Decoder行为，通过测试证明不是另一套缩水解析器。

`acquire`必须在运行时锁保护的线性化点完成绑定/活跃检查及一次短同步Host准入；获得服务但未claim不等于允许发送。内部管理和测试可替换注册实现以证明实际调用变化；Engine只拿受控lease，不暴露可绕过生命周期的裸Arc。状态视图包含实际活跃、准入、in-flight和恢复情况，不能把状态标记当实际撤销。

Rust codec签名按末节冻结；其余crate-private测试构造器由B先提供准确签名并与D同步，不允许双方分别实现互不兼容的接口。现有公开API兼容helper若保留，不得成为Engine绕过服务绑定的生产入口。

## 统一新用户验收

一个新数据目录、真实Host/CLI和本地受控Provider；fixture只能准备仓库/文件与mock服务，不可通过Store/SQL预注入配置、凭据、项目、授权、预算或Task。必须经公开初始化和业务命令：

1. 空路由启动，CLI安全输入合成凭据，查当前配置和原回执；配置前后Provider计数为0。
2. 创建并选择Project、配置明确工具/有限写权限、预算、网络预览与授权；权限上限不替代逐次审批。
3. 同一会话实际读/列/搜文件，拒绝一次写且无副作用，批准写后验证字节；批准Shell运行受控测试并核退出码/结果。
4. 消费事件并以游标续读；读取真实Git diff、变更和checkpoint，核内容与回执；外部修改冲突不覆盖；恢复成功后旧Task封存，通过新Turn继续。
5. 真实Host终止并wait，同库重启重新认证，验证配置/回执/事件/预算与文件事实保持，继续新Turn且不重复之前副作用。已完成重启与在途崩溃证据分别记录。
6. 明确Shell不能由文件checkpoint全部撤销，展示并测试实际恢复范围。不能为了拼链降低既有审批、上下文、恢复封存或预算不变量。

## 分工（以末节唯一文件登记为准）

- B：provider_runtime、provider协议适配、Engine装配与两处Completion调用、lib导出及必要模块内测试。
- D：独立Provider服务/生命周期/安全/取消和恢复测试，评审锁序、凭据与网络边界；发现B缺陷由B修复，不同时编辑共享源码。
- C：新增同一空数据完整CLI验收测试与当前操作说明，最终固定组合唯一五门禁。
- 经理：冻结接口与独立工作树，审查、串行整合、验收、进度。所有人继续原身份，NEXT-15完成后新任务ID另登记。

## 验收证明

既有Provider分块/工具/usage行为；真实Engine使用替换服务的可观测结果；缺失/错类型/版本/激活失败拒绝；codec超限/失败/panic；准入与disable两种确定交错；在途lease完成及效果撤销；无多发或未知账本释放；route check与Agent都覆盖；真实重启恢复；统一空数据CLI故事；旧HTTP/CLI/fallback兼容；最终固定check/test/fmt/clippy/wasm-check。

测试不能只证明新模块孤立可用。最终版本、原命令、退出码与日志必须对应实际组合。实现/报告/验收/合并分开记录。

## 经理接口裁决

采纳 B/D 只读评审，冻结以下边界：

- CompletionInput只含model、max_tokens、messages、tools。CodecError为不携带任意文本的固定分类。CompletionCodec提供同步encode与decoder工厂；CompletionDecoder提供feed、is_done和消费自身的finish，输出message/usage。全部object-safe且仅crate内部可见；既有Decoder用局部回调适配，不另造缩水解析规则。
- Host比较编码请求与规范期望的完整JSON语义：model、messages、max_tokens、stream、stream_options.include_usage、tools存在性/顺序及tool_choice，拒绝未知字段或改变输入。保持既有响应4,000,000字节限制和请求兼容范围，不以“大小校验”静默新设破坏兼容的阈值。
- ProviderRuntime先prepare：短锁取绑定/effect/generation，释放锁后执行codec编码、创建decoder与Host请求校验；得到的PreparedCompletion不是发送授权，不可Clone。acquire再持短锁重核相同generation与活跃准入，完成可失败lease准备后同步调用Host admit，成功才发不可复制CompletionLease并增加in-flight。disable/re-enable必须改变generation，旧prepared不能复用。锁外执行、取消及drop正确释放in-flight。
- Engine两处共同序列：prepare；Engine gate→runtime.acquire→短Store::network_admit；释放锁；lease执行Host发送和codec解码；Store::finish_completion。receipt由Host局部持有，不能藏在取消即drop的future里。服务prepare/acquire失败不产生Completion claim；已经claim后的异常必须finish，不能遗失占用。
- 发送前未发送的确定拒绝可按既有NotSent分类；发送后的解码失败、panic或取消必须Unknown，保留预约。非2xx仍沿原有界响应及Response状态。插件错误与panic不能包含任意内容；catch_unwind本身不足以阻止panic hook输出，必须有同步回调范围的安全hook策略或等效证明，禁止每次调用全局set_hook造成竞态。
- route check仍先进行Models并保留其calls；Completion缺失或拒绝只代表Completion未发送，不能谎称整个route check无网络。所有后续错误继续前置已有Models calls，保持模型不在列表时不发Completion。算术消息、max_tokens=128及tools空保持。
- 稳定provider id `builtin.provider-xpeach`，consumer id `builtin.engine-provider`，Host作用域，接口名 `provider.completion-codec`、kind Service、版本1.0.0。类型绑定、缺失/版本/工厂激活错误通过PluginHost；前置不可用沿现有安全Internal+NotSent输出，不添加未经协商的公开错误形状。
- D唯一新增 `src/provider_runtime_tests.rs`，B在lib以cfg(test)接入并提供仅测试用Engine/runtime注入；D不改B生产文件。测试必须穿过真实Engine/Host发送和finish，而非只验证孤立codec。C新增单一公开工作流测试文件，不改现有夹具来降低验收要求。

## 正式执行登记

基线为NEXT-15合并提交 `f2b5b6b071167372e9da14112890a3e48d11731d`；NoManCode tree `84f630de7154d12f2051796cd4d18fc50aa44258`，Rust tree `98d3f344d274cdff219c0d0047c4dae26cb23c5e`。两tree与受测候选完全一致，联合验收见[记录](../审查记录/2026-10-31NEXT-15联合验收.md)。原B-R14/C-R16/D-R13范围已验收，旧树与证据保留。

| 执行ID | 员工/分支/工作树（相对主仓） | 唯一产品写入文件 |
| --- | --- | --- |
| B-R15-01-20261031-E01 | B / codex/b/provider-runtime / ../nhc-b-provider-runtime | rust-app/src/provider_runtime.rs、provider.rs、engine.rs、lib.rs；各自模块内测试。确需panic守卫独立模块时可新增src/plugin_callback.rs并由B唯一导出 |
| C-R17-01-20261031-E01 | C / codex/c/first-user-workflow / ../nhc-c-first-user-workflow | rust-app/tests/first_user_workflow.rs；docs/cli-first-release-acceptance.md |
| D-R14-01-20261031-E01 | D / codex/d/provider-runtime-tests / ../nhc-d-provider-runtime-tests | rust-app/src/provider_runtime_tests.rs |

表中产品路径位于各树NoManCode下。Git模式为独立任务分支/工作树；各员工只提交本人授权产品文件。所有跨树merge、主仓暂存和整合由经理串行操作。人员档案只写主仓niuma，本轮报告/review分别用B第十五轮、C第十七轮、D第十四轮。不得在工作树更新另一套人员档案；读取现行任务和身份必须指向主仓，旧树中的历史任务板不覆盖本派发。

B先提供能执行的真实runtime/codec/Engine接线与测试构造器，冻结提交后供D合入；不以永久stub交付。D先按契约准备独立测试和反例，收到B准确接口即对接，有产品缺陷交B修复。C可以独立在已合NEXT-15公开CLI上实现完整故事，随后必须在合入B/D的最终组合上验证。B不编造空D测试模块文件；D文件尚未合入时不得添加无法解析的mod声明，经理合D后由B补接线或采用D已落文件的依赖顺序。

### crate内部codec签名

```rust
pub(crate) struct CompletionInput<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub messages: &'a [serde_json::Value],
    pub tools: &'a [serde_json::Value],
}
pub(crate) enum CodecError { Invalid, Limit, Failed }
pub(crate) type CompletionOutput = (serde_json::Value, serde_json::Value);
pub(crate) type Emit<'a> =
    dyn FnMut(&str) -> anyhow::Result<()> + Send + 'a;
pub(crate) trait CompletionCodec: Send + Sync {
    fn encode(&self, input: &CompletionInput<'_>)
        -> Result<serde_json::Value, CodecError>;
    fn decoder(&self) -> Result<Box<dyn CompletionDecoder>, CodecError>;
}
pub(crate) trait CompletionDecoder: Send {
    fn feed(&mut self, bytes: &[u8], emit: &mut Emit<'_>)
        -> Result<(), CodecError>;
    fn is_done(&self) -> bool;
    fn finish(self: Box<Self>, emit: &mut Emit<'_>)
        -> Result<CompletionOutput, CodecError>;
}
```

经理已核现有messages/tools为Value切片、Route.max_tokens为u32；现有complete返回(message, usage)元组，并不存在名为CompletionOutput的旧类型。上方别名新增于provider_runtime，不改变返回语义。类型名/可见性调整不能改变权限与生命周期语义。

panic防泄露可采用一次安装、委托原hook的全局hook及线程局部同步callback标记；只有受保护同步回调内抑制payload，RAII恢复标记，其它线程/正常Host panic仍交原hook。不能在每次调用切换全局hook，不得持Runtime/Store锁调用插件。若选择其它等价策略，报告证明范围；受信进程内边界不承诺抵抗主动修改全局hook的恶意native代码。

### 测试调度与提交要求

初始三人均可读取、编辑本人文件和执行轻量静态检查；先报告准确实现/测试计划与接口，再申请构建窗口。经理按B组件→C故事→D依赖组件→最终C全门禁调度，可依据实际准备情况调整，但不得同时重型编译。已授权组件窗口内的编译诊断修复可由原owner完成并保留失败证据，不能改测试断言掩盖产品缺陷；固定新候选后只重跑受影响检查。

全部工程、target、TMP/TEMP和原始日志留E；沿既有工具链离线、锁依赖，不安装/升级、不付费live、不release、不push、不清理旧树。沿已安装MSVC/SDK和build.ps1环境，不复用其它正在写入的target。最终日志绑定准确commit、两tree、源码清单、退出码和原始失败。

### 本轮验收编号

| 编号 | 必须证明 | 主要责任 |
| --- | --- | --- |
| P01 | 真实PluginHost依赖/类型/版本/工厂绑定，失败闭合；启动不直连回退 | B+D |
| P02 | 原Completion JSON与分块SSE/tool/usage/DONE/4,000,000字节边界兼容 | B+D |
| P03 | Agent与route check真实消费替换codec；Models回执在后续错误中保留 | B+D |
| P04 | prepare/acquire与disable两种确定交错、generation过期、在途lease完成与计数回收 | D |
| P05 | encode/decoder/feed/is_done/finish错误与panic安全，无payload外泄、无额外发送 | B+D |
| P06 | claim前无账本占用，claim后发送失败/取消/解码故障正确finish，unknown不释放预算；真实Host中断后恢复 | D |
| P07 | 上述同一新用户六步骤全链，不通过Store注入业务配置，单次副作用/事件/恢复/重启事实可核 | C |
| P08 | 固定组合完整五门禁及既有安全/迁移/HTTP/CLI回归；操作说明与实际功能一致 | C，经理最终验收 |

本轮不改变schema/HTTP协议/旧数据，若有必要先报告原owner与经理。项目书完整目标继续有效；P01–P08通过仅构成NEXT-16，不构成整个阶段一或全项目完成。

## 补充01：生产生命周期与首次组件顺序

经理初读B实际代码发现disable/enable/status仅cfg(test)，不满足生产生命周期验收。明确由B提供实际ProviderRuntime控制和公开Engine Rust管理入口及ProviderRuntimeStatus（active/admission/in-flight/effects/generation/recovery），测试wrapper复用同一逻辑。不新增HTTP/CLI/schema，控制为当前进程内状态，重启默认重新启用；不能宣称已有持久Provider插件管理。

acquire拒绝路径必须在释放runtime guard后析构codec的decoder，避免admit错误导致锁内Drop重入；D增加受控反例。同步factory/start/stop的panic输出也需被守卫，可信builtin wrapper内生命周期不能暗中调用任意插件代码且持运行时锁。原准备/准入/账本边界不变。

C已先交静态候选，当前调度改为C独立用户故事组件→B内核组件→经理合真实依赖到D→D独立组件→最终组合C五门禁；只有收到具体窗口者能重型构建。改变测试顺序不改变最终受测范围。

## 补充02：完整用户故事发现的Checkpoint resume错误分类

C首次固定e810979b组件编译成功，业务测试在恢复后resume旧Task时失败：预期conflict、实际request_failed，停止未重跑。经理独立读源码确认ensure_resume_unsealed先检查checkpoints并返回CheckpointError::Conflict，而server::resume只将ResumeError::Sealed映射为409，其余走通用400。没有证据显示旧Task实际被放行；缺陷是明确的封存错误丢失分类。

唯一新增写权给C：`NoManCode/rust-app/src/server.rs`中resume处理器的错误映射，识别CheckpointError::Conflict并返回409/conflict/retryable false，使用覆盖checkpoint/restore的固定安全说明。保持Store/Engine现有错误类型和事务，不扩其它HTTP路由。C在first_user_workflow同时断言创建Checkpoint后和恢复后resume均拒绝，拒绝不改变Task、Provider请求数或文件；不把预期降低成通用错误。

原attempt1证据保留，后续新候选另跑。B窗口期间C只静态修复，固定commit/两tree并重新取得窗口后执行；C最终组合包含此修正。此为P07真实发现并驱动的必要产品修复，不因原任务只有测试而忽略用户路径缺陷。

## 补充03：保留既有ResumeError封存文案

最终组合a362d8e9首轮check通过、test失败于既有restore_http_tests固定message/error断言。补充02把两类错误并为一个分支，改变了ResumeError::Sealed已有公开说明。C仍只在server::resume范围修正：该旧错误维持“任务已有恢复记录，不能继续原任务”，新增CheckpointError::Conflict使用检查点固定说明；两者都是409/conflict/nonretryable。不得修改旧测试预期、Store或Engine来消除失败。保留失败原件，固定修订候选后完整五门禁从check重起。详见[C首轮失败review](../../员工C/审查记录/第十七轮/2026-10-31最终门禁首轮失败与恢复文案修订.md)。
