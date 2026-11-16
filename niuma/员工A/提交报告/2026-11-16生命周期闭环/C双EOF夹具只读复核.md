# A 对 C 双 EOF 夹具的只读复核

日期：2026-11-16。A 未修改 C 文件、integration 或旧事故原件，未运行 Cargo、真实进程或旧 case16 脚本。本报告中的跨树路径以仓库根为基准。

结论：失败原件没有显示 A collector 的 EOF 观测被 Job 空门槛挡住；C 清除父 stdout/stderr 继承位的最小修法与代码机制相符，且保留原三项同时成立的断言。旧现场尚无逐项 signal/EOF 快照或子进程句柄清单，不能把继承泄漏推断升级为已证实根因，也不能把 failure-cleanup-confirmed 改为通过或 unknown。

## 固定失败证据

经理受测 integration HEAD 为 `6a11f8e8ac34ad8bfa90d432ce47c3220f1a6b7c`，产品 tree `2845d59085415d8dcfe20ff847b52e86a2de36fd`，Rust tree `284286b8247220b30b43df5f99afe4a10620e6c7`。`../nhc-lifecycle-20261116-integration/.local/lifecycle/integrated-targeted-2/source-before.json` 与 `source-after.json` 的 HEAD、source_sha256 相等，after status 为空。

`03-target-execution_authority_lifecycle.log` 记录18例中16通过、2失败，退出码101；本例报 `bounded lifecycle observation expired`。`fixture.rs::until` 的预算为20秒。result.json 的 `terminal_unknown=false`；这次实际测试用时45.10秒，不能将整个测试二进制时间写成本例时间。

现场为 `../nhc-lifecycle-20261116-integration/NoManCode/rust-app/target/lifecycle-fixtures/component-eof-DgdyMx/`：

|原件|只读观察|
|---|---|
|parent.ps1|与受测 `tests/support/lifecycle/descendant.ps1` 字节哈希相等；只设置子进程三路重定向，未清除父原标准句柄的继承位|
|ack、descendant-ready|均存在；按 fixture 控制流，ack 前已取得原父/后代 duplicate handle、读出各自身份并检查两者存活|
|identity.json|source 为 creator DuplicateHandle；parent 为 `[60536,134392927280970409]`，descendant 为 `[71284,134392927287165811]`。这些数值仅记录旧原件，不用于重开目标对象|
|cleanup.json|`state=failure-cleanup-confirmed`、`forced_by_fixture=true`、`retain_scene=true`；属于失败后原对象清理证据，不证明产品断言通过|

复算 SHA256：

- 原日志：`db27dda4d93f51a1f1f99e70c109bad9a2873aeb5fefee51b3ffa9e6c4a9292c`，与 result.json 一致。
- parent.ps1 与受测 descendant.ps1：`0478c0064775f68b97d02934e649d8c14a08bbf12d6b0780afed594f69953116`。
- cleanup.json：`89f6e8e06211fe638a55bcaf15cd4f8db15195b0b316df06f44c0c1f26c7ca69`。

## A collector 及原断言

受测 `src/process_lifetime.rs` 的 stdout 成功 reader join 后在1514行设置 `stdout_eof_joined`，stderr 对应1540行；1555行才调用 `accounting(job)`，1578行才设置 `job_empty`。两 EOF 标志不以 Job 空为前置条件。reader 的成功返回确实来自读到0或 Windows `ERROR_BROKEN_PIPE` 后完成，不是取消请求或线程启动标志。

`component_eof_keeps_lease_until_original_descendant_exits` 先等待原父 handle signal 与两个实际 reader EOF/join，再检查原后代 handle 尚未 signal，并断言 lease 未 settle、容量仍为1。原父 signal 和已完成 EOF 不会逆转，因此随后的后代存活观察仍证明三者同时成立。最后才发 release 并证明两原对象退出；不能用 Job 空替换前面的三项。

## C 最小修法复核

C 当前修改仅在后代 `Process.Start` 之前对 `GetStdHandle(-11/-12)` 调用 `SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0)`；检查无效 handle/API失败，再通过 `GetHandleInformation` 读回并检查继承位确已清除，保存 `parent-stdio-non-inheritable.json`。不关闭父输出句柄、不改变后代 Job 归属、不替换原对象 observer。只处理 stdout/stderr 足以针对此次双 EOF 目标，未要求扩大到 stdin。

`component.rs` 只给原等待追加失败诊断：原父/后代 signal、两 EOF/join 标志和继承位文件；原三项断言、lease/capacity 检查及成功路径均保留。A 已向 C 确认这份差异未见静态阻断。最终固定复核绑定 C `69226513d3aaaa4d48ea1dc76d96698c501a18a3`，已用 `git diff 65d32c36 69226513` 重新读取这两个路径，内容与先前审查/解析的草稿相同，两路径相对该提交无工作树差异。该提交内的另一个 Git 测试修正不纳入本份 EOF 审查结论。

机制旁证：[Microsoft SetHandleInformation](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-sethandleinformation) 说明 mask/flags 可清除继承位；[.NET v10.0.0 Process.Windows.cs](https://raw.githubusercontent.com/dotnet/runtime/v10.0.0/src/libraries/System.Diagnostics.Process/src/System/Diagnostics/Process.Windows.cs) 的 `StartWithCreateProcess` 在设置重定向后仍以继承标志 true 调用 CreateProcess，重定向本身不是句柄白名单。这是机制支持，不是对本机具体运行时二进制或旧现场隐藏 handle 的直接观测。

## 验证边界

A 对 C 草稿只执行 PowerShell AST 解析（0错误）和两相关路径的 `git diff --check`（通过），固定提交差异再次 diff-check 通过，不执行脚本、Add-Type、Cargo 或探针。继承位修正是否令原父退出/双 EOF/原后代存活在20秒内同时成立，仍由经理固定 integration 重跑证明；若再失败，新增逐项诊断用于区分父未退出与 reader 未到 EOF。

另外一例 `real Git diff not text` 由 C 诊断，本报告不据此改 A 生产组件。最终组件25例矩阵尚未运行；已有23/23只绑定旧82db固定组件。旧 B23 case16 unknown 继续保留，未改判、未重跑。
