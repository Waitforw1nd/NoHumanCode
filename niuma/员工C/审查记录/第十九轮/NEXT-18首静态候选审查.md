# NEXT-18 C 首静态候选审查

2026-10-31，项目经理。候选 `4b47a51b9a020cdfb7353f7d3efdb2703187a91b`；NoManCode `6d98b64b64aa6a593bf5a80b29dea7c895fed97f`，Rust `d0381550b515eded12b2b5ec1203979b15067a14`。已读五文件完整新增实现和测试及本人报告；C明确source/index clean/no-build，未合B真实依赖。

结论：CHANGES_REQUIRED，限下列静态修订与尚未完成的契约测试。没有产品动态通过声明，不把缺依赖导致当前不能编译当作新增C缺陷。

## C18-R01：客户端应拒绝不可能的运行态

优先级P2。`NoManCode/rust-app/src/cli.rs` 的 validate_runtime_component 只在非transition时验证 admission→active，因此如下Agent响应可通过结构与converged验证：active=false、admission_open=true、transitioning=true、in_flight=0、draining=false、effects=0、recovery=0、last_failure=null、converged=false。该状态与生命周期切换先关闭准入的约定冲突，旧Agent CLI也明确拒绝。

修订新客户端状态关系：两类型的transition均不得admission_open；Agent沿旧已知关系核draining/transition/active/in_flight，稳定active应有真实effect。Provider允许禁用且仍有已准入in_flight，不得误套Agent的draining约束。以经理正式契约与B实际状态机共同核对，若B需有不同合法中间态应给出具体原因，不能简单放宽客户端。

增加假服务正反例：上述不可能状态安全拒绝；合法Agent draining、合法Provider disabled+in_flight和transition旧effects快照可接受，完整u64计数不误按i64拒绝。写入遇非法响应按unknown，读取不打印不可信内容。

## C18-R02：公开准入与回放测试尚未证明M04/M06/M12/M13

优先级P1，验收缺口，C已如实报告。当前empty_database流程同时关两组件、重启、同时开启后才执行任务；它证明意图显示，不证明两开关各自实际拒绝，也不能识别Agent先拒绝遮蔽Provider失效。

在原真实空库Host/CLI流程中，分别让另一组件保持可用，创建任务并观察异步终态；确认原创建receipt仍存在、禁用组件阻止执行、Completion请求/网络/预算claim不增加。独立启用后实际mock请求恢复。files/shell均关闭的无工具Chat同时核请求的真实工具定义/任务设置，不靠测试名称说明无工具。

补公开跨四ID同key冲突、过期CAS、同状态新key递增revision和重放不驱动；真实在途任务disable后，新enable Busy且不消耗key，已提交key回放仍成功，旧任务可结束。底层更细屏障由D负责，C验证公开可观察结果。

目前truncated_write测试的fake handler未读取完整PUT正文，只计进入handler；补收到完整body后丢响应/断线、单次发送、固定unknown和原key原参显式回放取回原receipt。可用受控代理转发真实Host写后丢响应，或有真实可检查意图/回执的等效测试夹具，不能只返回硬编码成功值代替提交事实。

## C18-R03：M12/M14剩余边界继续实现

优先级P2，范围未完成。补HTTP方法/HEAD/Host、GET body、缺失/重复/合并幂等头、非法revision/非对象/缺字段、错误content-type与限额；CLI嵌套重复字段、超限/错误status-code/类型/秘密响应、JSON与human/参数exit。可表驱动覆盖，不为单个字段造大量重复测试。

Models-after-disable例已断言响应calls、计数及预算不变；需按call_id读持久网络回执确认真实保留，并补Balance独立受授权调用不受Completion开关影响。不要给错误路由/授权缺失造成的拒绝记作开关验证。

## 接续

允许C在原五文件范围解除source/index冻结继续上述静态修订和提交，no-build保持。B核心尚在实施，不能合入其活动草稿。C可继续不依赖B的新测试，准确内部接口依赖及时沟通。完成新固定候选及报告后经理增量review，再按实际完整组合放组件。原4b47候选及报告保留。
