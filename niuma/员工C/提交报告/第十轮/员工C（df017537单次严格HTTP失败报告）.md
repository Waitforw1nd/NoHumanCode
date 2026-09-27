# C-R10 df017537 单次严格HTTP失败报告

执行ID C-R10-01-20260927-E01，员工C / GPT-6-astra，2026-09-27。受测HEAD df017537e19ebadfd2c01a8264cff7a4784e7671；NoManCode e6773fec05f3aa3de81dcb1eb7a9f2580b1d84e1；Rust fdc7319c6beaa21f559e5485c0fdfaea9a1848f1；启动前clean。

真实命令：`cargo test --locked --lib restore_http_tests::partial_restore_committed_barrier_http_reopen_no_retry -- --exact --nocapture`。编译通过，exit101；0 passed / 1 failed / 0 ignored / 105 filtered。起止UTC原始记录见[C-R10证据/df017537-http-once.json](C-R10证据/df017537-http-once.json)。原日志保持原字节，发布副本路径相对化，双hash在[manifest](C-R10证据/df017537单次manifest.json)。

失败证据：src/restore_http_tests.rs:299 的 unwrap_err 得到 Ok(Task)，status=queued，消息已追加 must not retry。旧Task在partial恢复之后允许resume，构成新R04旧Task封存契约缺口；不是网络/grant失败假阳性。未授权修改生产，等待经理裁决。

此前已执行并通过断言：真HTTP三路径；A commit后独立DB可见complete；锁B得到ReadCurrent OS错误；A后suffix无C且无C副作用；receipt partial/restored1；A before、B/C after；锁释放后重复HTTP receipt不变。由于resume成功当场失败，后续newTurn typed封存、重开后的不重试/封存、调用计数不增断言均未执行，不能声称R04通过。

计数证据限制：测试确实在resume前读取Task/Turn/network_calls及provider计数与Task快照到局部变量，但该版本未输出它们，panic后值不可核证；日志仅能证明返回Task queued，不能据此声称Task/Turn数量或provider调用保持不变。未擅自重跑补数字。

环境：CARGO_BUILD_JOBS=1、DEV_DEBUG/TEST_DEBUG=0、CARGO_INCREMENTAL=0；target/TMP/TEMP分别为独立E树.local/target-r10、.local/tmp。既有MSVC来源D:/vsstudio/VC/Tools/MSVC/14.40.33807仅读取；Windows SDK来源机器C盘Windows Kits/10；项目产物只写E，不更新D。无release/live/push、无10次/五门禁；测试进程已退出。

下一步：经理裁决新R04与旧resume语义，若补范围由生产所有者修复；C不修改生产、不放宽失败断言。本次受控失败与860历史问题根因无直接证明关系。
