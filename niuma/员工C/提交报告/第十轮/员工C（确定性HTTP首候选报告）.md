# C-R10 首个可消费测试候选

执行ID C-R10-01-20260927-E01；员工C / GPT-6-astra，2026-09-27。契约1.0与任务包已完整读取。

实核接收：HEAD 4767f53971eedd73a8216b1c68801e6cdce8bd79，NoManCode 9800ad0ebcabf556c517cade04eaa8b433057ab6，Rust ca006f88165d346ee6e60910079e9dece53aff25；分支 codex/c/restore-http-deterministic、接收status/index空。独立树 ../nhc-c-restore-http-deterministic。本人独立index提交，跨树merge/共享index仅经理。

候选提交 71e9f77363a593c8e2d2c19c703abb5f53bff621；NoManCode dba63fe118e34cda68eba32fd53bc0e931c58333；Rust 4ef02a42d6ccb830d52185efb201cdd90c779514；提交后status空。

仅src/restore_http_tests.rs、tests/workspace_http.rs：旧不稳定partial及专用写锁探测移除，新测试真实注册/配置/grant/native Chat Task与loopback provider、真实HTTP三路径恢复。OutcomeCommitted(A)双向有界停点后独立SQLite连接验证complete持久；锁B让ReadCurrent得到真实OS错误，C未尝试；核partial/restored1、磁盘与DB；释放锁重复HTTP不重试，停止并join HTTP/provider任务后重新开Store/Engine并验证receipt/文件/封存。这是重开测试，不是crash证明。公开返回脱敏断言保留。

本轮仅运行直接rustfmt与git diff --check；没有编译、定向测试或五门禁。B真实probe接口/lib模块声明待经理合入，不写stub、不把未编译称通过。工具只读来源为机器C盘cargo/bin/rustfmt.exe；项目操作与TMP/TEMP均独立树E盘.local/tmp，无D写入。未release/live/push/reset/clean/stash。

待办：经理将此提交合B验证接口，经理再把完整候选合回C；C收到固定组合后先核SHA/tree/status，再定向10次，任一失败停诊断。最终R07五门禁仅另行冻结派发后运行。单次或重复通过不证明860历史根因。
