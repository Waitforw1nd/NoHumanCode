# D-R7-01 B组件第0.3轮开发review

2026-09-27，员工D GPT-6-astra，同D-R7。固定B f3bf333cb2ee48ba244477b6024f07cf33ff3fe2及已指定开发组合e40febf632548c0ad8cfb39556cf8a0e60745191；仅git show/diff，未跑测试、未写产品。未读取B/C变化态。C01/C02已有经理派返工，不重复作为新发现。

限定源码未见新增可证实阻断：

restore_observe宏先let result=$operation，再cfg(test)观察，最后返回同result；每个调用operation只执行一次，原?、map_err及is_err分支保留。FileAction闭包仅把原remove_file/atomic_replace结果封装为Result，原动作、失败后Unknown/Partial逻辑不变。非test构建不求值hook的engine/task/path/stage参数，仅保留operation局部result；无新自动重试。

emit_restore_probe先在完整let语句内持mutex、按Task过滤并clone Arc，语句结束临时guard释放，之后才调用callback。回调clear可重入，无probe锁跨回调；field/init/API/module/emit calls均cfg(test)。当前B尚未提交C lib声明属已知组合待办，不当已通过。结果类型不含借用DB guard：project验证/finish_restore_path返回拥有值/()，Store方法返回时DB mutex释放；OutcomeCommitted只在finish_restore_path成功后，原Engine gate继续持有。

error.chain按io::Error.raw_os_error及rusqlite SqliteFailure.extended_code提取，ErrorCodes仅Option整数，没有错误字符串/before内容。没有将Event写数据库/HTTP DTO或日志。现有部分业务映射本来会擦除底层错误，因此提取None不等于错误不存在，不能造码。宏包围的实际操作失败可准确报对应Stage；相邻checked==target和digest一致性ensure未单独发error事件，当前R03不据此声称所有业务拒绝都得到错误码。

B FileAction测试在ReadCurrent成功且latest_restore存在后才持share_mode(3)句柄，排除preflight；允许读写但拒删除，后续atomic_replace内部read仍可进行，MoveFileEx替换被真实句柄阻断。断言FileAction/io Some/sqlite None、unknown/restored0、原after内容未改、释放锁后不重试。SQLite对照用真实UPDATE trigger RAISE(ABORT)，断言OutcomePersist/sqlite1811/io None、文件已before而receipt unknown、无OutcomeCommitted与重试；未拿注入当860根因。

R01回调实取同Store receipt并用独立连接BEGIN IMMEDIATE;ROLLBACK验证无写事务，再clear hook证明probe mutex释放，另有无hook正常恢复。这里BEGIN IMMEDIATE是已知明确停点内的断言，不是旧式轮询猜阶段，因此不违反消除观察式barrier目标。该R01测试若将来误把callback搬进同Store锁，会同步死锁而非自主timeout；当前代码锁序正确，不作为当前实现阻断，但不能把测试本身称为可在任意锁序回归时保证有界退出。

结合C71e9须继续修经理已登记C01（preflight本来读取C，应用A committed后suffix/全量C无FileAction断言）及C02（sealed指定错误+DB/calls不变）。此前第0.2只读范围未见B，不能外推组合整体通过。最终cfg编译、正式OS/SQLite执行、C返工固定版和R05/R07均未完成于本审查。

下一第一操作：收经理固定完整B/C组合再R06，不读变化态、不跑门禁。