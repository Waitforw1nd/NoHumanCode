# C：full-1 runtime 回归适配

2026-11-16；经理明确新增两个文件唯一写权：`src/runtime_management_tests.rs`、`src/runtime_management_crash_tests.rs`。本批接续agent候选44acb33c，独立普通提交；不修改D接管的provider_runtime_tests.rs或任何生产核心。

只读经理 `.local/lifecycle/integrated-full-1/06-test.log`：global_key_conflict_and_independent_store_cas 在第二Engine构造处busy；intent_receipt_insert_failure_rolls_back_without_changing_local_runtime 未到目标屏障；sqlite_intent_commit_failure_rolls_back_before_runtime_changes 未触发预期COMMIT失败；两条crash例child提前退出。

已读保留原件：`tmp/nhc-runtime17-crash-vWanfK/migration.stderr.log` 是“migration probe did not block”，`tmp/nhc-runtime-intent-crash-krEazB/intent.stderr.log` 是“intent probe did not block”。两例父侧原本已drop Store；不能将其误报为父锁未释放。原代码注册hook使用裸SQLite普通路径，生产Store连接使用canonical路径，字符串key不匹配。

改动与断言：

- global_key_conflict_and_independent_store_cas：保持exact名称；只建立一个Engine controller，两个实际SQLite Store连接经open_shared共享该域。增加第二controller拒绝断言。全局跨组件key冲突及两连接同步CAS一胜一RevisionConflict保持；CAS在原Store::set_runtime_desired事务层执行，不用一个Engine本地gate把两连接竞争串行化。增加revision1、disabled及恰1条race receipt持久断言。该例验证持久CAS，其他既有Engine用例继续验证本地runtime应用，不能将这例宣称两个controller合法并发。
- intent_receipt_insert_failure_rolls_back_without_changing_local_runtime 与 sqlite_intent_commit_failure_rolls_back_before_runtime_changes：hook key从Engine的实际Store连接读取；故障trigger、真实deferred FK COMMIT失败回执、无receipt/无revision改变、runtime不变、同key重试及重启断言不变。
- migration_kill_and_two_fresh_process_reopens：注册迁移停点前以canonical路径打开原schema16 SQLite连接取key，不提前调用Store触发迁移；原9个迁移边界、原对象kill/wait、raw DB/WAL/SHM、旧DDL/rowid/密文字节、两次新进程重开断言不变。
- intent_kill_and_two_fresh_process_reopens：使用child Engine的Store连接取key；两组件的before/after commit原对象kill边界、事务提交事实、两次新进程重开与幂等回执保持。
- child启动隐藏窗口且test-threads=1，exact selector保持；若hook未命中，panic补实际操作结果供诊断。未延长原20秒观察预算。

本次单文件rustfmt、git diff --check通过；5个受影响exact名称保持。未执行Cargo、真实进程或测试。经理full-1的385 passed/51 failed/44 ignored、无terminal unknown是经理回执，非C运行；本批行为仍待经理下一窗口确认。
