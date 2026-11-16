# A 对 B 调用者交叉审

日期：2026-11-16；只读静态审查，没有修改B文件或运行Cargo/真实测试。首固定候选 `3e361b829ec9c5caaaaa6d8329c7854edcb32eda`，A组件当前固定 `aef3344c2c05c566514881b2c5538a73eb4cadca`。本审仅schema17生命周期切片，不是完整U/schema18验收。

## 已闭合的生产发现（绑定3e361b82）

|编号|问题与影响|复核结果|
|---|---|---|
|A-B1|generation先检查全域admission、后登记active，其他operation可在两个临界区间seal_unknown，仍放入新代次|`OperationScope::generation`现按operation→admission锁序，在同一admission临界区检查controller/open/cancel并登记active；静态闭合|
|A-B2|Git mutating已启动后超时/取消/超限映射Unavailable/Unsupported，会把可能部分写入的工作记Rejected|`git_worktree::run`现仅Exited+exit0是mutating成功，其余Settled(mutating)仍业务OutcomeUnknown；组件资源可正常settle、不误封整个域；静态闭合|
|A-B3|等待者Drop设置body_done，detached Git/Tool worker只克隆scope，可能在子进程结束而后台body未结束时减掉整项票|`OperationState.bodies`计数；`current()`在spawn前同步捕获带计数的ExecutionOperation，`inherit()`持到真实worker整体退出；只有bodies=0且无active代次才减整项in_flight；静态闭合|

共享Supervisor由实际域authority唯一持有，Engine动态配置保存后resize原对象；Shell/Git/diff通过 `run_process → wait_with_cancel`，没有新增launcher/第二owner。同步submit拒绝消费返回的原代次lease；Unknown在adapter同步关admission且计sticky unknown，迟到settle只清active；Git顺序代次不直接减整项票。原process/reader证据和最后协作线程容量由A组件承担。

## 最后增量需要复核

- A-B4（测试）：3e的原process observer尚未与原CreateProcess pid/creation交叉核对；部分故障曾统一映射reader失败、缩短预算可能只命中watchdog。A已提供原handle/identity、两reader真实join/EOF和exit_code_failed字段。B正在分别适配并保留旧s06原handle信号断言；不得将root_signaled布尔当独立对象证据。
- A-B5（错误传播）：git_diff最终一致性复核把 `OutcomeUnknown` 经 `map_err(|_| Conflict)` 擦成普通冲突。已给B保持typed unknown的最小修法，等待最后固定提交复核。
- A-B6（经理亦提出）：`spawn_task_worker`在 `operation.run(run(...))`返回后才保存最终Task/审批unknown标记/错误event，原body会过早结束。当前B草稿已以独立hold_body覆盖最后持久化和active移除，最终SHA到达后复核；不能把变动草稿当固定证据。

## 组件配合与边界

A发现旧组件Known通知早于watchdog/holder退出，容量1时下一顺序Git可能被拒；aef已在同一预算内等原holder线程signal再交付Known。Known的契约仍是外部process/tree/readers完成；若协调线程超界或线程wait查询失败，已证业务终态保留、容量槽继续占用，不能声称所有Known都证明holder退出。这一负向可用性边界已和C交叉讨论；未改为漏封门的假Unknown，也未释放未证明的槽。

经理已确认82db+e83旧固定集成check=0、23/23新组件矩阵通过；它不覆盖后续A增量和B接线。25例最终矩阵、真实Shell/Git/Host shutdown、业务unknown后的准入与持久化、observer故障覆盖均待经理固定integration验证。旧case16事故unknown不改判，不重跑旧事故脚本。
