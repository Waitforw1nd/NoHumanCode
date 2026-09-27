# NEXT-12独立Shell持久管理组件提交报告1.0

更新人：员工B / GPT-6-astra。日期：2026-09-27。任务B-R11-01，执行ID B-R11-01-20260927-E01。此报告属于当前实现任务，不替代已验收PRE只读建议，不恢复NEXT-10/11旧任务。源码冻结；本次只整理证据，未重跑产品测试。

## 交付与Git事实

独立树`../nhc-b-shell-management`，分支`codex/b/shell-management`。基线`6c84681f6ffc47afb521c472a181f6f0217f4fe3`，NoManCode tree `08cd78a59cad53f390c826c7484d21e3b820abed`，Rust tree `ef2d8775209065ceb11a34890feed50afa7e2e3b`，初始clean。源码提交由B执行；所有跨树merge、共享index与档案提交仅经理执行。

最终B候选`352a968551951791c3a3ce18d9a54a4d70941e2f`，NoManCode tree `d4997aebe5127ad984fc3aa69c8d6fd2a5ef83e3`，Rust tree `a8d23dd0aa189ac29fac69bba1eccb92c9ba851b`。交付时与本报告整理时status/index均空，无构建进程。

过程固定点：`6b879fce64d4be834018157c2cf70e7cb5bda717`持久层；经理合D487后`525ca53b267af292db7c78edae8a35cb0567c198`；`a24d231d8dd6ca88c294d2044e0a175073cc030b`双组Engine；`08f1c0a944b86b583292a7a719b5225cdad43716`真实审批/draining及S08测试构造；经理合D bd6后`76d2d7ebcbbd516f502843d198208eb712a89232`；最终352a968补S06/S09。经理合入的D文件不是B实现，不以合并归属冒认贡献。

## 实现范围

- repository/store：保留真12 DDL与验证；FK ON、Immediate单事务保存两表含rowid、拒绝额外trigger/index/FK及暂存碰撞、按子父顺序重建13、精确复制、增加shell true/0与新marker/version。填充失败及marker失败回滚；旧业务表/密文/旧marker不改。启动旧版本验证保持；新13验证完整约束和固定双ID。初始version/marker以同一读事务快照核对，迁移事务内再次读取。
- plugin_management：双固定ID、同一Store锁读取双desired、每组独立CAS revision、全局单列key；跨组旧key冲突，原receipt回放无协调。write_file复核files，run_command原claim Immediate内审批ack/项目检查后复核shell。
- Engine：两份持久状态先读后装配；两组锁独立，固定files/shell顺序；实际常量生成元数据；薄内存控制与持久管理分离。CommandOutcomeUnknown沿error chain在通用结果完成分支之前识别，返回UnknownResult而不落伪Finished/tool_result。
- 本方测试：公开plugin管理、schema迁移/回滚/CAS、Engine真实审批与命令、unknown传播和OS子进程crash。仅授权五旧fixture调整13marker/最终版本；不放宽原业务快照过滤。
- cfg(test)接口：经理批准new_with_runtime_for_test复用assemble供D真实失败runtime验证S08；本方per Engine/Task claim停点有界、生产不存在。S06仅消费D实际unknown_after_spawn_once，未复制或伪造lease/runtime。

## 矩阵证据及边界

| 矩阵 | B已执行事实 | 边界/其它执行人 |
|---|---|---|
| S01/S02 | 公开双插件元数据、独立effects/状态，双false重开无激活，单组启停不改另组，回放不协调 | D四服务/runtime registry及C Host重启证据另归各自；B未声称本项全部独立完成 |
| S03 | pending后禁用仍允许批准+ack事实，但NotStarted/零claim/spawn/tool事件/Provider续请求；durablefalse而runtimeactive直接claim typedUnavailable；tools/allow_commands/无ack/批准后Project撤权均阻断 | 无ack审批本身拒绝；不伪造批准无ack记录 |
| S04 | 真claim完成后有界停点，disable时in_flight1/draining/effects0，原命令真实marker/output/Finished/tool结果各一次，新acquire拒绝，files不变；正常cancel不杀已claim | 命令write_scopes为空仍合法写marker，符合shell不受文件write scope限制；不宣称OS沙箱 |
| S06 | D Task隔离一次故障在真实spawn后触发，实际收尾返回typedunknown；Engine保持Claimed、无Finished/tool_result/role tool、Provider调用仅1，resume拒绝，recover→Unknown不重试 | 此为注入传播证据，**不是实际OS收尾失败观测**；真实deadline/reader/helper边界由D证明 |
| S07 | canonical历史12 DDL + true/false非空业务fixture迁13，旧全表/密文/两表rowid保全，仅新增shell/marker，13重开无漂移；新库与DDL/marker/坏值/receipt损坏拒绝 | repository简夹具核两管理表rowid/marker与回滚；engine完整业务夹具核全部旧行/密文。两者均重建canonical旧DDL，**不是由旧二进制生成数据库**。未穷举全部损坏组合；缺shell/额外ID/孤儿独立case未由B单独运行，代码strict验证与全局FK负责拒绝 |
| S08 | FK开启填充/marker失败回滚真12、暂存清理/重试；extra trigger/index/FK/碰撞拒绝；跨组key冲突零desired漂移、独立revision、files双Store并发仅一赢、纯回放无协调、receipt失败回滚 | 两个已打开Store的files CAS有动态覆盖；**同时Store::open/schema13并发迁移没有动态覆盖**，不能以CAS代替。shell并发同ID及不同组并发专门case未独立运行；accepted与applyFailed真实联合test由D通过经理合入构造执行，不冒认为B运行 |
| S09 | **持久claim完成后、tool_start及首次poll/spawn之前**停点；父进程先读Claimed，再真实kill+wait exit1；重开recover→Unknown、无marker/tool事件、enable与resume不换grant/不重试 | 前次final的“claim前停点”为文字笔误。源码engine.rs:1507 claim返回后才执行1512起probe；测试kill前明确断言Claimed。英文claim-before-spawn表示claim先于spawn，并非before-claim。C负责真Host disabled重启与新Run完整恢复用例 |
| S10/S11 | B公开管理组件已跑；限定旧fixture已适配 | C HTTP/CLI与最终固定check→test→fmt→clippy→wasm-check，B未执行，不提前声明APPROVED或main已合入 |

## 全部组件命令与实际结果

所有工作目录为B树`NoManCode/rust-app`，由[runner](证据1.0/run-component.ps1)执行。TMP/TEMP为B树`.local/tmp`，target为`.local/target-r11`；dev/test debug=0、incremental=0、build jobs=2。MSVC仅使用既有机器安装，未安装更新。日志JSON保存实际cargo exit/UTC/当时HEAD及dirty列表。每次测试都是表中HEAD加当时未提交修改；不宣称最终352a968重跑全部组件。统计为每次执行通过/失败/忽略，重复运行不相加为唯一测试数。

| 日志前缀 | UTC开始 | UTC结束 | 实际命令 | cargo exit | 通过/失败/忽略 | 受测基线 |
|---|---|---|---|---:|---|---|
| engine-api-01 | 2026-09-27T04:58:27.8487632Z | 2026-09-27T04:58:38.7056120Z | `cargo test --test plugin_management -- --nocapture` | 0 | 3/0/0 | 525ca53b + dirty |
| engine-api-02 | 2026-09-27T04:59:32.3926274Z | 2026-09-27T04:59:34.0649809Z | `cargo test --test plugin_management -- --nocapture` | 0 | 4/0/0 | 525ca53b + dirty |
| migration-nonempty-01 | 2026-09-27T04:53:21.2175244Z | 2026-09-27T04:53:33.9944131Z | `cargo test --lib n12_true_schema12 -- --nocapture` | 0 | 1/0/0 | 6c84681f + dirty |
| shell-engine-01 | 2026-09-27T05:03:16.4316233Z | 2026-09-27T05:03:31.7135210Z | `cargo test --lib n12_ -- --nocapture` | 101 | 2/2/0 | a24d231d + dirty |
| shell-engine-02 | 2026-09-27T05:04:53.9800523Z | 2026-09-27T05:05:06.8352383Z | `cargo test --lib n12_ -- --nocapture` | 101 | 3/1/0 | a24d231d + dirty |
| shell-engine-03 | 2026-09-27T05:06:23.6467296Z | 2026-09-27T05:06:36.3798580Z | `cargo test --lib n12_ -- --nocapture` | 0 | 4/0/0 | a24d231d + dirty |
| shell-unknown-01 | 2026-09-27T05:10:27.6486093Z | 2026-09-27T05:10:41.5633782Z | `cargo test --lib n12_ -- --nocapture` | 0 | 6/0/0 | 76d2d7eb + dirty |
| store-01 | 2026-09-27T04:47:11.5297650Z | 2026-09-27T04:48:05.4973474Z | `cargo test --lib shell_schema13 -- --nocapture` | 0 | 2/0/0 | 6c84681f + dirty |
| store-02 | 2026-09-27T04:51:18.3311473Z | 2026-09-27T04:51:30.8660235Z | `cargo test --lib shell_ -- --nocapture` | 0 | 3/0/0 | 6c84681f + dirty |
| store-03 | 2026-09-27T04:52:29.0084033Z | 2026-09-27T04:52:41.2047647Z | `cargo test --lib plugin_management::tests -- --nocapture` | 0 | 4/0/0 | 6c84681f + dirty |

## N12-B01与其它过程异常

1. shell-engine-01：实际cargo101，2通过2失败。新夹具在Engine已失败后尝试failed→running；另一夹具误以为无ack可批准命令。产品正确拒绝。修为当前线程尚未让等待任务继续时对原running任务直接做typed claim检查，并断言无ack审批本身拒绝。
2. shell-engine-02：实际cargo101，3通过1失败。夹具先撤销Project权限再批准，批准入口正确返回project_permission_revoked。改成批准+ack后同步撤权，再让任务进入最终准入，证明最终检查阻断。未放松生产权限。
3. shell-engine-03：4/0，相关夹具修订通过；shell-unknown-01包含这些原case并新增S06/S09，6/0。N12-B01提交员工修复证据，最终review闭合由经理决定。
4. store-01出现两个新Store读取方法尚未消费的dead_code warning；后续测试与Engine使用后消除，原warning日志保留。
5. PATH旧rustfmt 1.7.1格式化产生与现有style不同的噪音；尝试`rustfmt --edition 2024 --style-edition 2024 ...`返回Unrecognized option。改用已安装`rustup run stable rustfmt --edition 2024 <owned files>`（1.9.0）恢复正确格式，未更新工具链。该非产品命令没有独立raw日志/精确UTC或内层exit记录，不伪造；会话输出保留。
6. 编写时文本替换曾误命中repository前部两个normalize比较，首次编译前已还原；未将该未编译状态算测试失败。一次工具编排在成功获取shell-unknown-01结果后误引用functions变量，ReferenceError；未影响cargo0、未重跑测试或改源码。此类无独立raw日志工具异常仅作会话事实登记。
7. CRLF/LF Git提示不当作产品测试失败；各次git diff --check未发现空白错误。它们无单独UTC/raw文件；不等同正式fmt门禁。

## 未执行与风险

B未运行最终全量check/test/fmt/clippy/wasm-check、五旧目标完整回归、tool_session_turns专项或限定Clippy；此前“随后补回归”是计划不是通过，经理当前明确最终全量回归由C承担，本次不新跑测试。无release/live/push、无工具升级、无reset/clean/stash。没有最终352a968 clean状态下额外产品重跑。

当前in_flight表示admitted lease/worker及有界收尾数量，不证明OS后代全灭；Host强杀不保证worker finalizer执行。Shell命令不受文件write_scopes限制，不是OS网络/目录沙箱；禁用不取消已claim执行。typedunknown不自动重试。S06注入与D真实OS/pipe测试必须一起评审，不能互相冒称。

## 证据清单与交接

[manifest](证据1.0/manifest.json)逐文件记录原始/发布SHA256、字节数、每项路径转换。10组log+JSON和runner共21对，原始位于本人树`.local`且未改写；发布副本以B_WORKTREE/MSVC_ROOT占位替换绝对路径，移除BOM，保留换行，替换次数全部列出。JSON中状态/命令/exit/UTC不改。人员报告仅主仓niuma；未改冻结源码或共享index。

下一操作：经理review固定352a968全部diff、整合D/C并由C独占最终五门禁；B仅响应明确返工/证据核对，不自行重跑或改冻结源码。当前报告不声明任务APPROVED。
