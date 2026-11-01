# NEXT-21W2 补充04 P01 attempt8 动态结果

2026-11-01，员工 B。按经理 [最小诊断窗口](../../审查记录/第二十一轮/2026-11-01W06-P01最小诊断窗口.md) 执行，未启动 W05 或其他组。固定候选 `32aa43d24a586ffb295474b4eea515fc84412f33`，产品树 `021c8a6c8c2be5dd97c8c841a425b0cad058813d`，Rust 树 `b1ffa2472a0e16e569a85c41a029b8567031916f`；独立树 `../nhc-b-wasm-worker-process`，原始现场在该树 `NoManCode/rust-app/.local/next21w2-b-32aa43d2-p01-attempt8/`。旧 attempt 原件保留。

## 结果与边界反例

三步均 exit 0：offline/locked probe build、lib test no-run、exact 单线程诊断；最后一步实际运行 1 例，0 failed，392 filtered。目标 worker PID 64492，probe 读完 EOF、进入并放行等待，达到内存分配失败错误 1455、exit 43。持目标观察句柄 wait 0；Host 阶段记录 process wait 0、Job active 0、三路 I/O 收割、collector stopped 和 run handles released；无 collector shutdown unknown。独立树结束 clean，结束重型进程清单为空。

同一次真实 W2 Job 的额外对象 **PID 9276** 构成当前现场边界反例：

- `p01-stage.log` seq 13–14：NEW 通知后取得的当前对象句柄 PID 9276，创建时间 FILETIME `134379770799583295`，映像 `C:\Windows\System32\conhost.exe`，存活 wait 258，`IsProcessInJob` 对本 Job 返回 true/1；目标锚前后均存活且成员为 1。
- seq 15–18：scan 4 首次列表为 `[64492, 9276]`，逐对象检查及第二次列表/记账显示两个对象，active 2；额外对象的同一已持句柄在 seq 17 仍为 wait 258/成员 1，目标前后亦存活且成员 1，随后 scan complete。实际配置 active limit 为 1，Host 自身处于某个 Job，flags 为 8458。
- seq 19–20 为目标内存限制消息；seq 29 中该额外对象的已持句柄 wait 0。Job 最终 active 0、采集器确认退出。seq 22、25、29 的映像查询分别报错 5/31/31，不能抹去或改写，但 seq 14/17 对象活跃时的映像查询成功。

通知和列表并非原子：后开句柄不自动等同旧通知对象；这里的结论依实际持有的当前对象句柄、创建时间、当前 Job 成员、两次列表和目标前后状态，而非仅凭通知或名称。该现场已证明本 Job 层级同时有两个活对象，不以 `conhost.exe` 名称豁免；不将其身份反写到 attempt7 的 PID 12952/21912。诊断通过不等于 P01 或完整 W2 验收。

## 固定工件与证据

`logs/` 保留每步命令、退出码、完整 stdout/stderr、stage、状态、环境、源码与工件清单；`modes/p01/p01-memory.stage` 为 probe 阶段原件。probe 固定及模式副本 SHA-256 `70f6b7de19eaf2311f949fe449254d077e3bd2bc591c46bbbac99cb5b7edce90`；实际 lib test 执行文件和固定副本 SHA-256 `1bac0dda2375361ec4652a71f8a5ce7f7c6ef03771f5b7a12d3df85ecbf6fe49`。测试 Cargo 输出指向与 no-run 相同的实际 test 文件，最终两份源文件、快照和模式副本重新散列一致。189 条产品源码前后清单 SHA-256 均为 `e1e9944f229dd4143fa940731e23d0b14dcaa2e9a51bfef90862df3c335bab93`。

## 供经理裁决的最小方案

查 [Microsoft Process Creation Flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags)：`DETACHED_PROCESS` 控制台进程不附着父控制台，不得与 `CREATE_NEW_CONSOLE` 同用；`CREATE_NO_WINDOW` 与 `DETACHED_PROCESS` 同用时前者被忽略。可研究仅将当前 `CREATE_NO_WINDOW` 替换为 `DETACHED_PROCESS`，不叠加这两者，其余 `CREATE_SUSPENDED`、Unicode 环境、扩展启动信息、三路显式 stdio、句柄白名单和 Job 配置保持；这只是候选，不由名称推断已消除额外对象，也不承诺其他系统环境无辅助对象。

候选须先经经理核授权及静审，再在新现场验证真实 W1 与故障 probe 的三路管道正控、目标映像和资源/退出清理，以及独立实时 Job 对象记录；实际发生多成员或观察失败仍作边界问题/unknown。尚未修改生产 flags、未建立新动态窗口；W05、其余 W06 保持开放。
