# B：full-1阻断修复候选

2026-11-16。经理full-1前置18+4+1定向通过，其后lib结果385 passed / 51 failed / 44 ignored；失败原件保留。B只读整合树`integrated-full-1/06-test.log`定位，在本人树修改并普通提交，没有修改integration或执行Cargo/真实探针。

|候选SHA|实际修复及保留约束|
|---|---|
|`62ae726fb43bcd323ffe5b90254d690dcd2a4533`|共享Store::open和open_shared复用repository::verify_initialized_schema；同一deferred事务核user_version、12个schema6–17标记、schema6完整结构/event、7/8/9/10/11/17/14/15/16全部既有验证。schema17只覆盖当前plugin/runtime，14/15/16另核worktree/budget/config。未调用带索引修复的verify_current_schema、apply_schema或recover。budget四项原拒绝断言保留并增加open_shared拒绝；真实崩溃例增加活controller恢复拒绝且receipt不变，再drop/recover/重建核同一receipt，原kill/wait与请求hit计数保留。|
|`f00910fb1f24d146777dcc14fd6dc572d1867ae7`|resume在保留Host关闭消息后，只读检查durable claimed/unknown，返回原ApprovalError::UnknownResult；任何后续执行仍经过authority.check。没有放宽全域封门。Engine内嵌fixture使用真实原scope、先恢复后装controller、退出旧controller后恢复；活Host观察改明确只读SQLite，保留原claim/kill/wait/重启断言。restore probe隔离例使用独立合法域，新增同域第二controller拒绝。Git filter正控解析绝对git.exe并显式env，保留真正marker写入与生产拒绝对照。|
|`a2685f9cf524ccf17960277e5d981107648725f5`|Git collector恢复cfg(test) next_command/git_spawn_begin；真实Outcome::Settled后记录原identity PID和exit的git_wait_end，只有Exited标ok；Unknown、未启动及失败保留失败记录。没有降低W08/worktree诊断固定序列、命令数量或退出码断言。隔离child补齐cwd及显式完整环境再覆盖测试注入；F04整个真实ToolLease回调位于真实Store/Engine原操作scope。|

A固定审查62ae726f确认两种共享重开入口完整只读验证、未引入迁移/恢复，静态未见阻断；此结论不代替动态通过。B各批均单文件rustfmt及git diff检查通过。schema仍17，完整U事务fence和W3 bootstrap不在返工范围，run_wasm拒绝保持。

经理重新分配后，B不修改`worktree_management.rs`（A内嵌tests）、`agent_runtime_tests.rs`与两份`runtime_management*_tests.rs`（C）、`provider_runtime_tests.rs`（D）。W08故障根因在B的Git诊断事件丢失，修复位于git_worktree.rs，未修改server断言；A对应worktree管理监督计数也复用本次事件恢复。

下一验证须以经理新整合SHA/tree进行受影响库全量，再六门禁。以上是针对实际失败的候选，不声明51项已全部动态修复；其他角色候选和后续错误继续按唯一文件所有权独立提交。
