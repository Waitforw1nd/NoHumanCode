# B：C审批与原进程夹具固定交叉审

2026-11-16，员工B（Wegener）。固定审查对象为 C `67336f94cd134dcf2faa58063f61f9605ce71b65` 与 `63d9ca8c7ed32e3be3c597a60319a0230ddd0b56`。已检查提交差异、相关完整函数及执行权计数实现；本次仅提交自有报告和独立状态，产品与其他角色文件均未修改。

结论：在本次指定的 runtime_stop、原 Child 证据和恢复边界内，未发现新增静态阻断或原业务断言弱化。

- `approval_gate.rs` 的真实命令用例仍先观察 counter 一次实际落盘，再销毁 runtime；没有改用 Engine 优雅关闭来代替该中断。保留原 Store observer，等待 admission `(false, 0, 0)`，确认原票已收束且无域 unknown 后才继续。底层票数只有在 body 全部结束且 active generation 为空时下降，不是仅等待任务列表为空。
- 资源收束后仍断言审批为 Claimed；释放 observer 后，恢复将其置为 Unknown。原 typed `ApprovalError::UnknownResult`、counter 仅一行、任务非 completed、无 tool_result 均保留。域资源已收束不等于业务结果已知。
- 等待 helper 的 admission 元组不单独证明 controller 已析构；恢复入口仍通过原 `claim` 拒绝活动 controller，没有绕过生产门。12 秒到期或发现域 unknown 时写证据并保留当前 Store/进程等待经理监督，不执行恢复、猜测清理或进入下一用例。这是明确的 terminal unknown 保全分支，不是超时后算通过。
- `support/owned_child.rs` 从 spawn 到退出始终持同一个 `std::process::Child`。`output` 只在原对象 `try_wait` 返回退出状态后读日志；超时/观察失败保留原对象。`kill_wait` 对原 Child 执行 kill，并等待原对象报告退出；kill 失败的竞态也必须由同一对象确认退出。没有按 PID 重开、枚举或任意 PID 终止；Drop 复用同一路径，已观察退出时不再终止。
- CLI 保留第二实例 exit=1、原中文 busy 消息以及原服务仍可读取审批的断言。network 环境凭据测试先释放父 Engine，再串行启动真实子进程；新增 exact selector 确实运行一例的检查，保留 reject/confirm/send/rotation 分支、两次真实请求计数及最终 Stale 断言。

验证边界：B 未运行 Cargo、编译、测试或真实进程探针。本结论是固定 SHA 的静态交叉审，不声称所有异常路径已动态覆盖。经理最新回报 full3 九目标通过、全量正在 HEAD `0fb20585` 运行；此处记录为经理回执，未将正在运行的全量写成通过。当前保持 schema17 首切片产品冻结，完整 U/schema18 与 W3 不混入。

下一步：仅接收经理全量实际失败证据，按当前唯一写权处理阻断；继续由经理持有唯一验证窗口。
