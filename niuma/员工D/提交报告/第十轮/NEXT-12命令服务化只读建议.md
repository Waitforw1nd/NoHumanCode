# NEXT-12 命令服务化只读建议

更新人：员工D GPT-6-astra；日期：2026-09-27。任务 D-R10-PRE01，执行ID D-R10-PRE01-20260927-E01；新独立只读预审，不恢复D-R9实现。

基线：main `ae86cf84b863bf42c2db85a46ff9aa22a1034fe1`；Rust tree `ef2d8775209065ceb11a34890feed50afa7e2e3b`。实核两者匹配、开始时 `git status --short` 空。Git模式：固定提交只读；本轮无源码提交执行人，档案整合人项目经理。仅写本报告与本人身份，未运行测试/门禁、未改产品或共享暂存区。以下行号均对应固定基线，路径以仓库根为基准。

## 结论与最小装配

推荐独立 `builtin.shell` provider 与 `builtin.engine-shell-tools` consumer，服务键 `tool.run_command@1.0.0`，插件初始1.0.0。保留files四服务与其consumer；不得把shell requirement加入原 `builtin.engine-tools`。`plugin_host.rs:1142/1183`表明stop拒绝活动依赖者、stop_subtree会撤销传递consumer；若共用同时require files/shell的consumer，关闭任一provider就使另一工具集失去consumer绑定。无须为本片改动PluginHost通用算法。

在tool_runtime内部推荐两个明确装配状态，分别拥有admission/apply_failed/recovery与AtomicU64计数；可共用现有短Mutex，但Host若共用必须按两ID过滤instances/effects/recovery。当前 `tool_runtime.rs:264–291`统计整个Host效果，直接加入shell会污染files四服务状态；更简单的备选是同一短锁下两个独立PluginHost。两方案均须冻结唯一选择，不做运行期可选框架。files/shell管理互不撤销绑定、互不清除失败记录，任一初始化失败不得误关另一个。

支持经理files状态版本1.2、shell1.0建议；区分状态协议版本与plugin manifest版本，勿隐式连升。files affected四服务，shell affected run_command，compat均仅run_wasm；是否对旧状态响应有兼容分支由经理冻结。`tool_runtime.rs:489–576`现有run_command直接创建WorkspaceService并不计数；必须完全移出compat fallback，禁用时definitions与acquire都拒绝，不能只隐藏菜单。

## 命令执行所有权与边界

现有 `workspace.rs:340–410`的timeout只包child.wait，随后await stdout/stderr无期限；reader JoinHandle drop会detach；timeout中的taskkill.output无期限、kill结果忽略、reader只abort未await，且错误文案无证据声称“已停止”。具体反例是直接pwsh已退出而继承管道的后代仍持有输出句柄，执行可超过timeout而悬挂。

推荐新增 `command_execution.rs`承载命令过程，workspace的run_command只保留最小委托，不改文件算法。保留tools/allow_commands、工作目录策略、8192长度、1–600秒/默认30秒、隐藏窗口和现有环境处理；捕获上限仍64KiB，若改变超限后的排空策略须明确，不暗改输出语义。

shell lease的execute在首次poll、首个await之前，将owned任务快照、args、service和计数guard转入私有tokio worker。caller只持结果接收端，不能持有可abort worker的句柄；caller future被drop/abort只放弃等待，worker继续至命令结束或有界回收。计数guard必须归worker，而不是仍留在外层execute栈；否则表面spawn worker仍会提前归零。正常Engine.cancel不杀已准入命令：`engine.rs:1804`仅取消token，`1533/1648`当前perform_tool不select取消，保留这一行为。停用shell只关新准入，已取得lease继续执行；禁止持runtime锁spawn、await或回收。

使用同一绝对执行deadline覆盖child.wait和两个reader的完成。任何wait错误、reader错误/异常退出、deadline到达均走唯一finalizer，禁止 `?` 提前离开而遗留另一个reader。直接child必须kill并wait；reader必须abort并await确认任务结束。如保留taskkill/T，给helper独立受限期限，并对helper自身kill+wait，不能把helper无界等待移到新模块。执行期限外可增加固定cleanup budget（建议5秒，经理可在冻结时调整）；所有回收等待共用这个budget而非每步重新计时。

有界收尾结束但无法确认直接child回收时，返回明确cleanup-unknown，不声称已停止；按经理本轮补充，建议冻结crate可见typed error `command_execution::CommandOutcomeUnknown`：已spawn后无法确认执行/回收结果，或owned worker结果通道消失，均返回该marker，不能只靠字符串匹配。Engine须在通用Err→tool error→finish路径之前识别它，保留Claimed（当前按unknown对待、reopen转Unknown），停止继续Provider，不自动finish/reclaim；否则未知执行会被伪记Finished。in_flight表示Host内lease/worker的执行和有界收尾数量，不是OS全树存活计数；归零不证明后代进程全灭。已超时且直接child退出、reader取消完成也只能证明这几个对象已收尾。Host强杀/runtime teardown不保证worker运行finalizer，本片不承诺Job Object sandbox或强杀后全树清理。若要求“未确认清理绝不归零”，需另设sticky cleanup状态；不能同时承诺无界真实进程跟踪与有界worker结束。

## claim与执行空隙必须冻结

`engine.rs:1473`在runtime同步admit内claim，`tool_runtime.rs:540–576`在admit前完成可失败快照、再增加计数，顺序应保持。审批等待不占lease；权限与registry复核先于claim；claim成功后才可spawn。

claim成功不等于spawn或完成：`engine.rs:1638`的tool_start持久事件失败可直接return；调用方也可在首次poll执行future前丢弃lease。因此不能将所有claimed承诺成必已启动，不可因内存count0重新claim。此空隙维持durable unknown/不重试，由经理Store契约覆盖。即使worker已完成，caller不再接收或最终结果落库失败也不能凭内存成功推断持久完成；worker不自行越过Engine调用finish_approval，也不自动重试。正常结果仍通过 `engine.rs:1648–1687`原finish链。重启读到running/unknown与管理desired恢复必须分别处理，不能以shell重新enabled重放命令。

构造函数须先同时取得两持久desired状态，再暴露runtime；旧单bool构造兼容语义（files bool、shell默认true或必须显式传入）需明确命名与测试。管理继续runtime→Store短同步回调，禁止Store→runtime反锁。新control/DTO应限定固定两个ID，保留旧files接口包装及N10-D01首次失败/显式false清旧Failed的语义；schema13与global key保全由经理负责冻结，D不在本报告预认其实现。

## 必须验证的反例（建议项，未执行）

1. files停用/失败/卸载后shell仍可真实执行，shell停用后四文件服务仍可执行；双向effects/count/recovery各自归属正确。
2. shell停用先赢则claim回调零次、无spawn；claim先赢则停用呈draining，原lease可完成，新acquire拒绝；审批等待时停用，批准后仍不可偷走compat路径。
3. 受控真实命令ready后abort caller，shell count仍1，直到release/超时及收尾完成才0；测试必须有有限watchdog、实际worker完成确认，不能只sleep再看数字。
4. 直接child退出但管道后代仍开、stdout成功stderr失败、child.wait错误、taskkill/helper超时等路径都在冻结总期限内收尾；reader abort需join证据，不能靠测试runtime退出代替。
5. claim后spawn前丢弃、spawn后caller丢弃、结果落库前失败，以及真Host kill+wait再重启均不自动重跑命令；未知态不等于“命令没执行”。Host恢复测试不得声称证明OS后代已灭。
6. 正常Engine.cancel保持已claim命令运行语义；timeout输出不得把清理失败描述为已停止。全量门禁另由经理指定人员执行。

本轮只读建议已完成；无实现授权、无本次测试结论。后续第一操作为接收经理完整任务与最终冻结接口，再核基线/唯一所有权。报告与身份更新后冻结；旧树、原始日志、历史贡献全部保留。