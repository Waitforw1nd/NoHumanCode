# NEXT-05 联合只读支持：B 固定候选 8df22e3

支持人：员工D / GPT-6-astra；日期：2026-09-27；沿用 D-R3-01-20260927-000811。经理明确授权继续只读支持，不重开产品任务，不改源码、不重跑正式测试，不扩大矩阵。最终验收由经理裁决。

受审提交：`8df22e3664b6d1a773661a2b3879ee2b284ec853`；比较起点为已审 WIP `50826f4c8762ff881ea6c0c27a2fc5c834d34045`。审查开始时 `../nhc-b-tool-turn` HEAD 与固定提交相符，`git status --short` 空。范围是 store/engine/tests 新差异，重点 D-N05-01、N07/N09、production runtime→claim→lease 及 unknown/replay。

## 结论

D-N05-01 的修复在固定代码和针对性夹具证据范围内闭合；本次未发现新的产品阻断。N09已从D模块callback推进到真实Engine/Store/文件路径，两个线性化方向有针对性证据。N07新增真实生产gate排队验证有效，但不能把单个join用例描述为全方向强制并发屏障。以下意见是限定只读复核，不替代经理对B历史/事务主体与最终联合门禁的验收。

## D-N05-01：删除中断文本放行例外

`src/store.rs:1816` 的 `chat_continuation_prefix_tx` 对需要审批但找不到同源approval的历史调用，统一在第1821行返回 `ChatTurnError::UnresolvedEffects`，不再接受中断补齐文本作为副作用已解决的证据。

`tests/tool_session_turns.rs:499` 分别构造 write_file/run_command 的合法闭合历史与同源tool_result事件；结果文本正是旧resume产生的中断占位。断言精确 UnresolvedEffects，Turn/Task数不增长，Provider请求数不增长，因此不是因无关格式/外键错误而通过。此用例明确是持久化形状夹具，不是运行旧二进制并真实崩溃；原[静态来源分析](NEXT05联合只读支持-B-WIP-50826f4.md)继续提供旧路径可达性依据。

`tests/tool_session_turns.rs:302` 对历史pending、approved/not_started、claimed、unknown、prepared/unknown change、claimed/partial/unknown restore进行夹具覆盖。审批状态循环同时断言已提交key仍回放原receipt；本轮代码仍在Engine和Store入口优先classify幂等，因此新key拒绝与旧key回放不冲突。这里不宣称各故障均由真实进程终止产生。

## 生产 runtime、claim 与 lease

- Engine构造持有唯一ToolRuntime，disable/unload委托同一实例（`src/engine.rs:114`）。Provider每轮获取该实例definitions（第674行），调用前再次检查同实例definitions。
- 只读Allow通过acquire获取lease（第818行）；需审批工具仅在等待结束且Approved后进入acquire，回调调用真实 `Store::claim_approval`（第831行）。等待期间没有lease。
- `perform_tool` 接收不可Clone的lease，原workspace gate、before/preflight、tool_start与finish事务保持；最终调用 `lease.execute`（第1012行），不是检查registry后直接绕回workspace分发。
- runtime锁内完成active payload验证和claim，返回后释放同步锁；perform_tool再等workspace gate，不持runtime锁跨await。当前所审路径未见Store持锁反向进入runtime。
- Store claim仍把claimed/unknown返回 `ApprovalError::UnknownResult`，不重新授予lease；运行时对象/lease没有持久化恢复通路。D-N05-01补齐的是无记录历史，而非削弱原unknown语义。

## N09 证据充分性

`src/engine.rs:1683` 的 `n09_claim_wins_stop_and_duplicate_claim_never_gets_a_second_lease` 持有真实Engine workspace gate，在真实审批批准后等待数据库execution_state=claimed；这时perform_tool被挡在FS前。随后经生产execute_or_wait尝试重复领取，再停用/卸载文件插件，核definitions撤销，释放gate。最终断言审批finished、恰一workspace change、文件为after-0、恰一tool_start和tool_result。这同时证明已admitted的lease不会被停止强杀、后续工具不再获得执行路径；不是仅测试一个bool。

该用例重复领取目前只断言is_err，而非downcast UnknownResult。静态核对夹具的Task/call/args均来自真实claimed记录、runtime仍活跃，且无FS尚未发生，实际应进入Store的claimed→UnknownResult分支；因此本轮不将其判为产品阻断。证据表述应保留“错误类型由源码核对、未有独立typed断言”的边界，不能把单个is_err夸大成分支类型实测。

`tests/tool_session_turns.rs:465` 在真实pending审批时先停用/卸载，再批准；最后Task失败、approval仍NotStarted、零change、文件保持before，覆盖停止先胜零claim/零FS。`tests/tool_session_turns.rs:679` 另验证下一轮真实Provider请求中无read/write定义，Provider强行返回revoked-read也零tool_start/tool_result。与D已验收的effect撤销/真实payload模块证据合用，可以支持N09限定结论。

## N07 证据充分性

`src/engine.rs:1832` 的 `n07_send_and_restore_wait_on_same_production_gate` 真正锁住Engine gate，把restore与send futures分别poll到Pending，确认无restore记录/新Task。先排队restore、后排队send，释放锁后验证恢复complete、后继Task有Host恢复事实、磁盘在后续批准前为原字节、后继完成且仅一新change。该证据有效覆盖“restore先进入gate，send之后消费恢复事实”。

`tests/tool_session_turns.rs:632` 的同keytokio::join证明单Engine序列化时两调用收敛一个Turn/Task、一个replayed；join本身没有保证同时卡在写入前的强屏障，不单独宣称双DB并发。已有session_turns中的双Store/同前序屏障、running-resume/旧候选快照检查保留，结合生产gate源码和新增restore/send用例支撑N07。活跃scope冲突测试等待另一个真实writer进入pending后send，断言冲突、无新Turn和Provider调用，是有效独立前置条件。

## 证据读取与未执行项

本轮只读固定差异/现有源码与B日志，未运行产品check/test/fmt/clippy/wasm-check，也没有改B/D源码。已核对B以下三份证据原始/发布两端SHA-256与其manifest全部一致：

- [commands.jsonl](../../../员工B/提交报告/第四轮/日志20260927/commands.jsonl)
- [tool-session-second.raw.log](../../../员工B/提交报告/第四轮/日志20260927/tool-session-second.raw.log)
- [tool-session-regression.raw.log](../../../员工B/提交报告/第四轮/日志20260927/tool-session-regression.raw.log)

B记录的 `cargo test --locked --test session_turns --test tool_session_turns --lib` 退出0，日志为80个lib + 19个session + 10个tool-session通过，含本报告引用的N07/N09用例；这是B实跑、D核读，不冒称D复跑。commands同时保留早期check/test的101失败及后续通过；本报告没有覆盖掉失败历史。最终联合五门禁仍由经理安排，真实Host重启N12不由这些对象/线程夹具替代。

本轮只读命令与hash脚本退出0；探测B工作树不存在的.local/报告路径曾返回非零，随后按共享发布manifest定位原始证据并核验成功，非产品构建失败。D实现候选仍为已限定验收的 `3d00f121b0c66124cbb2afc2d6fdf0d1cf504c10`，无需改动；此支持报告新增落盘，旧报告/日志字节保留。
