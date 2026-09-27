# NEXT-12固定组合只读交叉review 0.1

2026-09-27，员工D GPT-6-astra；D-R10-01-20260927-E01接续只读。固定b5b1abcfd2a45c37fa38aad6d955a92de09359bf，实核NoManCodebaf2884a035efcad10850b537aa6f3939b356abf/Rust1f0cbc1bb77f9195e98af22c2403548124a869c6。全部从本人E树git show读取，不checkout/merge/改产品/运行测试。本人HEAD5e74afc及clean保持。仅本报告与身份写主仓。

限定结论：CLI当前四版本映射/目录完整性、C S09真实Host流程、B S06/S09 unknown与claim停点链，源码层未见新增阻断或确认的假通过；不是最终联合验收，b5b1abc不称为已最终五门禁受测。经理已登记N12-B02验收测试补缺另由B执行，本review不把尚缺反例当作已经验证，也不重复标为已确认生产bug。

## CLI与C当前目录

src/cli.rs:719的匹配同时限定plugin ID与version，精确支持files1.0/1.1/1.2和shell1.0；affected/compat/dependent三组数组严格等值，desired ID必须等当前ID。当前files和shell的compat仅wasm，shell dependent不共用files consumer。:869目录只接受旧单files1.0、旧单files1.1或顺序精确files1.2+shell1.0；重复、缺项、跨代、错序不会被逐项合法性掩盖。

status分支再核请求ID；set分支核receipt/current请求ID、enabled、expected与新revision关系、current revision不倒退。未知/无效PUT结果归operation_result_unknown，未见自动重发；非收敛响应先完整输出再返回既有失败码。human shell明确无文件write_scopes/OS出站目录沙箱、禁用不取消已准入命令。

tests/plugin_cli.rs新S10正控当前目录/两个status/shell变更及human说明；反例含缺项/重复/错序/跨代/错consumer/错ID/version，请求次数核无自动重发。旧版本正控是mock DTO表示兼容，不是实际旧Host二进制；不宣称旧CLI懂新Host。元数据校验不等于认证远端runtime真实存在，该边界不在此次变更范围内。

## C S09真实Host

tests/plugin_http.rs:383 launch真实CARGO_BIN_EXE_peachsh，ready有期限。S09先通过CLI持久shell=false，:1013明确child.kill+wait再启动新Host；重启desired与原receipt一致、admission=false/effects0/inflight0、DB snapshot不变且Provider请求0。后续三次独立Run分别是恶意command拒绝、files四工具正控、显式enable后审批+ack真实命令正控。

disabled反例核任务failed及精确未授权错误、审批空、tool_start/result0、文件marker不存在、Provider只一次无续请求。files正控核read/list/search/write四真实结果与写文件；enable后先无ack失败且marker不存在，再ack成功，核真实shell文件/stdout/exit0及单个审批approved/finished/acknowledged，总Provider数精确5。每阶段files完整status保持不变。此测试的零Provider结论特指重启尚未新建Run阶段，不能扩大成后来显式新Run也零Provider。

通过路径确为kill+wait，不是drop Arc当crash；失败/timeout辅助路径主要kill_on_drop兜底，不声称每个失败分支都有wait实证，也不把Host终止扩大成所有OS后代清理。产品无JobObject保证仍保留。

## B S06与S09

src/engine.rs:1505先真实acquire/Store claim，随后仅cfg(test)按Task选取shell_claim_probe，复制callback后释放probe锁，再停点；位置在perform_tool/tool_start/spawn之前，生产没有停点分支。:1694在通用Err→finish前沿error chain识别CommandOutcomeUnknown并返回ApprovalError::UnknownResult，避免把未知命令误记Finished。

:3720 S06使用D真实spawn后一次性注入，核Approved+Acknowledged事实保留、Claimed、任务failed、无tool消息/result、一次tool_start/Provider、lease0，resume typed拒绝；启停不改业务表，重开recover转Unknown且grant/Project绑定不换，第二resume仍拒绝。它证明typedunknown传播与恢复，明确不冒称观测OS清理失败。这里drop Engine发生执行已经idle之后，仅作重开/恢复测试，不作为真正crash证据。

:3801 S09启动真实当前test exe子进程，子进程真实审批ack/claim后原子写claimed.json并在有界停点阻塞。父进程读DB确认Claimed、marker不存在，ChildGuard.stop执行kill+wait；重开recover转Unknown/interrupted，授权/Project绑定逐表等值，启停和resume不改变业务快照、无busy/marker/tool消息及tool_start/result。因此证据准确为含真实Engine的测试子进程在claim后、spawn前死亡恢复，不是生产HTTP Host在任意命令执行阶段crash。命令副作用未发生的结论来自明确停点+零tool事件/marker，不把Claimed本身当作已spawn证明。

n10_rows刻意排除plugin管理表/schema_migrations，适于证明启停不改业务数据，不是全库不变或管理receipt零写证明；报告措辞须保持此域。生产CLI Host kill+wait由C S09另覆盖，两份证据组合而不互相冒称。

本轮没有新产品阻断；未执行任何测试、未核最终门禁新证据。经理所报65对既有hash属于经理核验事实，本人未重计为测试。报告/身份冻结；下一第一操作只接固定后续补证或明确返工。