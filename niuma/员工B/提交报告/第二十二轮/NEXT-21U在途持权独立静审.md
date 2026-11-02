# NEXT-21U 在途持权独立静审

2026-11-02，员工B，经理终审/C唯一实现。本轮仅静态，不编译、不运行测试/探针、不访问网络；C草稿未固定，不作为完整U验收。

## 证据基线

实际模型 `gpt-6.1-terra/high`；B原树 `a4366b56cc88644336d45f961313f294b05ea5ec`、产品tree `4935f1aa104de548c97a26043cb0d5e442e24dea`、Rust tree `bfea730396003f4aa675cc25dcfaa1c6b39f1e14`，clean/index空。C树 HEAD 前后均 `a799abc1eb10c7ee42bbcc39e227ff3bfb4d56ac`，含未提交草稿。UTC 2026-11-02 00:15:35.416955–00:15:35.469794 采集12个文件到 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/u35-b22-static-snapshot/`，每文件采集前/副本/采集后 SHA256一致；后续再次只读核这12个C源仍一致。连续采样非文件系统原子快照，不能排除采样前后或中间的瞬态变化；结论绑定副本，不声称代表C最终候选。副本 manifest SHA256 `08b8e3bbff086838d936235e7adc0096742c30f6ab02c9b198dcf18e906ba1e7`，逐文件相对来源、长度及hash见该manifest。

主要证据文件：`command_execution.rs` SHA256 `3796da927112539092ac41cbe10e190675a6b9a8b81fc23481f0bf51c701189f`；`tool_runtime.rs` `1a7751410126654fac29d52fde7625450aa8a54a85e5becb8304a72562968849`；`provider_transport.rs` `6758ad80a5c314289234b49edc07f6a541d9faadbbeecd4598ec00795645b0b7`；`execution_authority.rs` `8ff242b7f0f40ff33425a394e92e36992e717465a5a8333f731372ec26d3405e`。以下行号均指该副本 `NoManCode/rust-app/src/`。

## P1：Shell 直接进程和管道收束被当作全部工作收束

### 经理指定边界的再次独立核对

收到补充后再次只读当前C树：HEAD仍 `a799abc1eb10c7ee42bbcc39e227ff3bfb4d56ac`；`command_execution.rs` 与该HEAD无diff，Git blob `4933060aafbccf875efaddafa5ccd69f0f785e87`，SHA256 `3796da927112539092ac41cbe10e190675a6b9a8b81fc23481f0bf51c701189f`；`tool_runtime.rs`、`tool_call_application.rs`、`execution_authority.rs` 当前hash分别仍为本报告快照所列值。所核代码与上述证据副本一致，C的其他在途文件仍可能继续变化。

逐分支核实：成功分支只要求 `child.wait()` 和两 reader `JoinHandle` 返回，reader各以 `take(65536)` 限长，达到长度也可返回而无需观察底层真实EOF；满足后 `collect` 在 `command_execution.rs:195–196` 返回 `Ok`，`ToolLease` 的 `tool_runtime.rs:823–828` 直接settle。超时分支在 `:207–225` 仅对直接Child try_wait/kill/wait及reader abort/join，`completion` 真超时且此清理成功时 `:230–233` 返回普通 `bail!` 而非 `CommandOutcomeUnknown`；`WorkspaceService :63–75` 给该错误附 `SettledToolWork`，ToolLease同样settle。其他内部收集错误若 `completion.is_ok()` 则走typed unknown，不能将所有错误都说成普通超时。

建议同一真实后代的精确交错：真实Host审批执行Shell，测试helper创建并回传一个后代的创建身份，后代的标准输出/错误显式重定向至独立文件，持外部释放门保持活着；父测试持该后代原进程对象。情形A令直接PowerShell正常退出，情形B令它保持至超时后由原清理路径杀并等待；均先核该**同一后代对象**仍 `WAIT_TIMEOUT`，同时直接Child已signal且Shell调用已返回/普通超时。然后释放第一个Engine及全部Store/其他票据，尝试第二owner获取同域；若现实现接受，再放行该同一后代写唯一标记，便证实旧工作与新owner交叠。安全期望是未证实树归零时保守unknown/第二owner拒绝；随后通过测试持有的原后代对象有界回收，分别验证已证明全树终态后的正控。每个情形由隔离夹具独立设置，后代身份不得仅靠PID复用猜测；测试未执行，当前是代码支持的反例推断。

`command_execution.rs:79–95` 通过普通 `tokio::process::Command` 启动 PowerShell，`kill_on_drop(true)` 作用于直接 Child；代码未建立或查询后代所属 Job。`command_execution.rs:163–196` 仅等待直接 Child 状态和两个 reader 返回，即返回 `Ok`；reader 的 `take(65536)` 在达到上界也可返回，不要求后代退出。超时分支 `:198–233` 只 kill/wait 直接 Child并 abort/join reader，成功后返回普通错误。`tool_runtime.rs:63–75` 把非 `CommandOutcomeUnknown` 的普通错误标为 `SettledToolWork`，`:820–830` 对 `Ok` 或该标记调用 `ticket.settle()`；`execution_authority.rs:1161–1167,1185–1187` 随之解除 armed 并减少in-flight。

可证伪触发：批准的 `run_command` 启动一个受测试夹具控制的后台后代，明确把其 stdout/stderr 重定向到独立文件或空设备，然后让直接 PowerShell 正常退出；后代保持等待并稍后写项目标记。直接子进程及两管道收束使 Shell 返回 `Ok`，外层把票结算；调用返回后释放Engine/Store，第二owner可能获得同域，而后代仍能写旧任务工作区。超时变体中先让直接 PowerShell保持至超时，后台后代独立运行；直接进程被回收后普通超时也可能被标记 settled。该推论基于可见生命周期和无树证明，不声称已在本轮实际生成或观察过后代。它与C已在处理的 **Git** 后代问题是相同证据类型但不同的 `run_command` 执行链，现有Shell普通错误分类不解决它。

最小方向：由C为Shell真实工作引入可证明从创建到完成的进程树边界/等待凭据；例如在允许条件下将初始进程创建挂起、先绑定不可逃逸的受控Job再恢复，并以原句柄和Job成员终态证明结束，实际兼容性由C/经理评估。不能建立或无法确认该边界时保守返回 `CommandOutcomeUnknown` 并保留票；对确实未创建进程的参数/启动失败及证明全部工作收束的普通错误仍可settle。不能只观察PID或直接进程的非零/退出，也不能简单把所有错误永久unknown。

建议受控反例测试（只提供设计，未运行）：真实Host/审批路径启动受控helper，helper创建独立后代并显式解除/重定向两输出通道，发送自身PID/创建身份与已启动回执后让PowerShell退出；父测试持后代原对象，并用显式释放门让后代在允许时才写唯一标记。先证明直接进程已signal、两个reader已结束且后代原对象仍未signal，再关闭Engine/Store：只有已证明整个树归零后第二owner才能取得域；无法证明时须unknown并拒绝第二owner。独立正常前置拒绝、非零且树归零、超时树已回收作为可继续正控；超时且后代保活作为unknown负控。测试夹具须自己对原后代对象有界终止/等待，无法确认时保留现场并停窗，不按名字猜杀。

## 其他路径静态观察与待验证

- Provider：`provider_transport.rs:273–320,366–376` 发送前arm，响应体由独立worker持票，channel容量1，consumer Drop时send失败后继续排空至EOF；读取失败/超限及worker丢失时armed Drop转unknown，EOF才settle。`provider.rs:264–272` 和 `provider_runtime.rs:605–615` 保留DONE后调用返回。该结构对所列路径未发现同样明确的提前释放，但静态阅读不能证明runtime shutdown、传输异常或边界时序。建议真实本地server分片测试：DONE返回后停body，关闭Engine/Store仍拒第二owner，EOF后允许；consumer Drop后EOF正控，截断/超限/worker取消及runtime shutdown负控，测有界通道和请求数不变。若消费方保留stream但长期不poll，worker可阻塞在有界channel发送而暂不能继续读EOF；这是背压/活性风险，不能单凭此认定提前释放或无界内存，宜用受控停读/恢复与取消测试量化。
- ToolLease：`:808–822` 先完成任务绑定再把ticket移入独立worker，worker启动后arm；调用方取消不直接取消该worker。Shell还有 `:850–863` 的嵌套spawn，若外层worker在内层仍运行时被runtime取消，armed Drop应保守unknown；WASM `workspace.rs:368–376` 的blocking join被中断同理。仅从控制流可作此判断，尚无本轮动态证明；建议分别在内层Shell启动后、blocking closure进入后设置可控屏障，取消调用者及关闭runtime，验证新owner被拒，闭合后只在有终态证据时恢复；不得把task取消视为操作已终止。
- `WorkspaceService` 对同步文件普通返回、已完成blocking join 的错误分类有对应代码；`WorkspaceEffectUnknown` 与work settled分别处理，未发现本次快照中可证伪的同类提前释放。Store事务和Engine关闭仅沿本路径核对，非全入口审计。

本轮不重复Git成功路径/普通错误或recover/controller已反馈事项；上述P1待经理复核并交C按原任务修订。所有建议测试均未执行，W2已合主线与此静态审查无混同；W3生产绑定及项目后续仍开放。
