# A 对 B 调用者交叉审

日期：2026-11-16；只读静态审查，没有修改B文件或运行Cargo/真实测试。首固定候选 `3e361b829ec9c5caaaaa6d8329c7854edcb32eda`，A组件当前固定 `aef3344c2c05c566514881b2c5538a73eb4cadca`。本审仅schema17生命周期切片，不是完整U/schema18验收。

## 已闭合的生产发现（绑定3e361b82）

|编号|问题与影响|复核结果|
|---|---|---|
|A-B1|generation先检查全域admission、后登记active，其他operation可在两个临界区间seal_unknown，仍放入新代次|`OperationScope::generation`现按operation→admission锁序，在同一admission临界区检查controller/open/cancel并登记active；静态闭合|
|A-B2|Git mutating已启动后超时/取消/超限映射Unavailable/Unsupported，会把可能部分写入的工作记Rejected|`git_worktree::run`现仅Exited+exit0是mutating成功，其余Settled(mutating)仍业务OutcomeUnknown；组件资源可正常settle、不误封整个域；静态闭合|
|A-B3|等待者Drop设置body_done，detached Git/Tool worker只克隆scope，可能在子进程结束而后台body未结束时减掉整项票|`OperationState.bodies`计数；`current()`在spawn前同步捕获带计数的ExecutionOperation，`inherit()`持到真实worker整体退出；只有bodies=0且无active代次才减整项in_flight；静态闭合|

共享Supervisor由实际域authority唯一持有，Engine动态配置保存后resize原对象；Shell/Git/diff通过 `run_process → wait_with_cancel`，没有新增launcher/第二owner。同步submit拒绝消费返回的原代次lease；Unknown在adapter同步关admission且计sticky unknown，迟到settle只清active；Git顺序代次不直接减整项票。原process/reader证据和最后协作线程容量由A组件承担。

## 最后增量历史问题

- A-B4（测试）：3e的原process observer尚未与原CreateProcess pid/creation交叉核对；部分故障曾统一映射reader失败、缩短预算可能只命中watchdog。A已提供原handle/identity、两reader真实join/EOF和exit_code_failed字段。B正在分别适配并保留旧s06原handle信号断言；不得将root_signaled布尔当独立对象证据。
- A-B5（错误传播）：git_diff最终一致性复核把 `OutcomeUnknown` 经 `map_err(|_| Conflict)` 擦成普通冲突。已给B保持typed unknown的最小修法，等待最后固定提交复核。
- A-B6（经理亦提出）：`spawn_task_worker`在 `operation.run(run(...))`返回后才保存最终Task/审批unknown标记/错误event，原body会过早结束。当前B草稿已以独立hold_body覆盖最后持久化和active移除，最终SHA到达后复核；不能把变动草稿当固定证据。

## 组件配合与边界

A发现旧组件Known通知早于watchdog/holder退出，容量1时下一顺序Git可能被拒；aef已在同一预算内等原holder线程signal再交付Known。Known的契约仍是外部process/tree/readers完成；若协调线程超界或线程wait查询失败，已证业务终态保留、容量槽继续占用，不能声称所有Known都证明holder退出。这一负向可用性边界已和C交叉讨论；未改为漏封门的假Unknown，也未释放未证明的槽。

经理已确认82db+e83旧固定集成check=0、23/23新组件矩阵通过；它不覆盖后续A增量和B接线。25例最终矩阵、真实Shell/Git/Host shutdown、业务unknown后的准入与持久化、observer故障覆盖均待经理固定integration验证。旧case16事故unknown不改判，不重跑旧事故脚本。

## 最后固定94305558复核

复核提交：`943055581d73803b1adbb1921e8f15d8d4ffc7d3`；以 `git show` / `git diff 3e361b82..94305558` 读取固定内容，不以B变动工作树替代证据。结论：**A-B4/A-B5/A-B6在该固定源码中静态闭合；本次delta范围未见新增生产阻断，动态待经理窗口。**

|编号|固定源码复核|结论|
|---|---|---|
|A-B4|`command_execution::Probe::capture_supervised`从A复制的原handle取得observer，使用原CreateProcess保存的pid进入`ProcessObservation::from_handle`并核实际GetProcessId，再核observer的GetProcessTimes creation与原creation相等；原`original_exit`仍以该handle的WaitForSingleObject检查。root_signaled仅补充观察，不替代原对象信号。reader_error、wait_error、cleanup_unknown分别映射实际故障且分别断言stdout_failed_joined、exit_code_failed、stdout_reader_held命中；恢复原预算，保留Job空、双reader join与stderr EOF断言。|静态闭合；命中与收束的真实执行仍待定向窗口|
|A-B5|`git_diff::read_with_barrier`最终validate_root/head/index_entry/work复核使用`review_conflict`，只把其他既有错误映射Conflict，OutcomeUnknown原样传播。|静态闭合|
|A-B6|`Engine::spawn_task_worker`在run前捕获`hold_body()`，保持到typed/claimed unknown归类、最终`save_running_snapshot`、失败event写入、active移除全部返回后显式drop。`spawn_worktree_worker`及`ToolLease::execute`也在spawn前捕获额外body，命名`_body`持到真实后台结果处理/发送完成。`hold_body`增同一scope计数且不在drop时取消，`finish_if_ready`仍须bodies=0且无active代次才能减原整项票。|静态闭合；覆盖最后持久化及错误返回，未另建owner|

顺带核对94305558的shutdown汇总使用域in_flight/unknown与既有background状态组合；未将Epoch/SQL成功或协作者future Drop当进程完成证明。本轮不扩展schema18/全入口fence审查，也不把静态闭合称完整U通过。

经理已说明94305558与A25清单/C65d32c36进入新固定集成定向窗口，A没有启动或重复任何重型检查。本复核无新增返工请求；后续若动态失败，以经理固定日志驱动最小修复。

## shell cwd 固定 bc291abe 追加复核

受经理后续指令，只读复核 `bc291abe04afc2bc4c5156b4a09b373ea2547690` 及其父提交差异；不改产品、不运行Cargo或真实测试。结论：**本次shell cwd差异未见静态阻断，实际修复效果待经理full-1。**

`command_execution::execute` 仍先canonicalize任务workspace；绝对PATH条目中的可信pwsh解析、workspace内exe排除仍对原canonical root执行。新增 `shell_cwd` 仅转换传给ProcessSpec的cwd拼写：接受Disk/VerbatimDisk前缀及RootDir，逐个保留Normal OsStr组件重建本地drive绝对路径，再要求新路径canonicalize等于原root。转换/核对失败发生在run_process取得generation并submit之前，不会把已启动进程误归未启动。没有拼接/改写命令文本，也没有更改输出JSON、取消链、Supervisor或数据域owner。

拒绝UNC/设备路径的范围与既有 `workspace_identity::validate_path` 支持的本地Disk/VerbatimDisk根相符；canonical二次核对避免普通Win32拼写落到不同路径。该比较不是新增的原目录handle pin或全文件系统竞态保证，不能将它扩称为沙箱。长路径、特殊名称及非Windows行为没有在本轮静审中执行验证，转换失败按启动前普通错误保留。

新增 `shell_pwd_join_path_preserves_dotnet_paths_with_spaces` 使用带空格目录，沿测试包装创建真实Store/Engine operation，实际命令保留 `$PWD`、`Join-Path` 及.NET写文件，断言exit0、stderr为空、PWD无provider/verbatim前缀、canonical仍为原根且文件内容正确；在解包结果前仍用原observer检查退出。删除的仅是无人调用的 `Probe::capture_process` 包装，`ProcessObservation::capture` 与 `s06_original_handle_rejects_live_and_query_failure_accepts_exit` 的原handle正反例仍保留，supervised observer仍核pid/creation。

同提交的 `execution_authority` 测试增量把Git脚本字符串改为raw literal，保留PowerShell引号和换行转义；`ReaderRelease` 在错误返回或展开时也只打开故障reader的测试释放门，原组件继续负责真实排空、join与容量释放，不把释放请求当已收束。A按固定差异执行diff-check通过并已向B同步。经理已将本提交与C69226513合入full-1，后续full与组件25例窗口串行归经理；旧23/23不能覆盖此候选。
