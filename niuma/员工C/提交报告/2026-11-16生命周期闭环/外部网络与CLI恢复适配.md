# external-1 网络与CLI适配

证据来自经理运行 integrated-external-1/01-external-tests.log；C仅静态读取，没有运行Cargo/真实进程。

- network_authorization::receipt_contradictions_are_corrupt_and_result_commit_failure_keeps_same_unknown_id：网络返回已结束，保留PersistenceUnknown/Claimed断言后，明确Engine.shutdown settled、drop旧Engine，再以Engine::new_recovered取得唯一controller并恢复。原same-id Unknown和TCP次数2断言不变。
- network_authorization::environment_credential_reference_and_value_are_rechecked_across_real_processes：配置后父Engine正常shutdown settled并drop（含Store/domain锁），四个原子进程阶段仍reject/confirm/send/reject；每个child结束前shutdown settled，四者结束后重开Engine读取原authorization Stale。保留stored引用、环境值轮换、TCP次数2断言；新增exact命中1测试、exit/stdout/stderr完整诊断。
- cli::cl08_check_and_server_startup_remain_compatible：真实失败是第二Host已exit1，但busy诊断由原中文变成英文，不是父Store未释放。原中文/exit1/仍可连接原Host/审批not_started断言不变。B确认在核心file.try_lock边界恢复中文兼容；本候选依赖该核心提交。
- tests/support/owned_child.rs：两个文件共用窄测试辅助，隐藏窗口、保留原Child、stdout/stderr文件，无名称/PID清理。CLI check/prepare/contender各15秒观察，网络nested child30秒观察；观察超时/错误保留原进程及Child并写unknown.txt/NHC_TERMINAL_UNKNOWN停窗。CLI原Host在正常完成或断言展开时仅用原Child终止并最多5秒确认终态；未确认同样保留现场。

原测试名未改，无删断言、无新增公开产品接口。单文件rustfmt（cli、network_authorization、support）及git diff --check通过。运行验证待经理；不声称旧失败已实测修复。host_configuration两文件未修改，归A；runtime/session归B，HTTP归D。
