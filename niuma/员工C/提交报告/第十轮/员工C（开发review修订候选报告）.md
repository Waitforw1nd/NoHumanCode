# C-R10 开发review修订候选

2026-09-27 员工C / GPT-6-astra，执行ID C-R10-01-20260927-E01。提交6500f8166e7cb99b2c96c831b352a68b369327b4，独立树提交后clean，仅本人两文件。

N09-C01：区分preflight与OutcomeCommitted(A)之后suffix，允许C预检；suffix无C事件且全量无C FileAction/OutcomePersist/OutcomeCommitted。N09-C02：封存断言改为明确恢复相关错误、ChatTurnError::UnresolvedEffects；对比Task快照、Task/Turn/network_calls计数和真实provider请求计数，无增加。删除unused domain::*。

只读发现当前resume实现可能允许恢复后的旧Task继续，仅写被封锁；严格新断言将揭露R04契约缺口，不因旧is_err笼统通过而掩盖。未修改生产，待经理合B固定候选后先真实运行一次，若resume成功即停止并上报；不通过增加重试求绿。

格式工具纠正：首候选在repo根运行rustfmt --edition 2024，命中了默认1.82工具链的rustfmt1.7.1；本轮在rust-app读取工程stable工具链，rustfmt1.9.0（48a229ceae 2026-09-01）显式edition2024，修正本人两文件样式。TMP/TEMP均E盘.local/tmp；工具安装只读使用，无D盘项目操作。已执行rustfmt与git diff --check；未编译/测试，无运行中进程。经理可普通merge B组合。
