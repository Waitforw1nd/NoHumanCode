# HTTP 收束与并发修复

2026-11-16，员工 D。源码候选 `57fc5eaf37dc4ced22be674597221fa7eba3c848`，前置 `a5f82bf1402c4a9b1d3af78e2ac016fa9d404e34`。本轮只改 `NoManCode/rust-app/tests/approval_http.rs` 和 `NoManCode/rust-app/tests/turn_http.rs`，经理已确认整合；未修改核心实现或集成工作树。

## 变更与保留断言

- ah06 real command：保留原真实命令和副作用到达屏障。原 runtime 关闭后保留原 Store 与 Weak<Engine>，观察实际 `execution_admission_status == (false, 0, 0)`，确认旧 Engine 消失及原 approval 仍为 Claimed，才恢复原库。没有先 graceful shutdown 改写 claimed→unknown 的崩溃恢复语义。最终 HTTP unknown、decision conflict、一次副作用和无 result/no resend 断言保持。
- ah14：原 current-thread runtime 内完成批准与 claim 后不让出执行权；销毁该 runtime 并通过相同 owner 屏障，重建真实 HTTP 服务后检查 unknown、精确 conflict 消息及单次 resolved。第二 runtime 正常路径 shutdown settled，并 abort+await 原 server。
- 收束 helper：12 秒内轮询的是实际域持有状态，不以固定 sleep 推测结束。unknown、观察错误或到期均写 `http-restart-unknown.json` / `NHC_TERMINAL_UNKNOWN`，同测试进程保留原 Store observer 并停驻，不重开 controller、不按 PID 杀进程、不把到期当死亡。
- n11：两个真实 HTTP listener 共用唯一 Engine，原同时提交、201/200、单次 provider 发送与五表各增长一次断言保留；第二同域 Engine 必须被精确 busy 错误拒绝。另用两个独立 Store 连接和线程 Barrier 竞争同一新 key，真实 `append_chat_turn` 事务必须一新建一重放、回执指向同一 turn/task、五表各增长一次，且无额外 provider 发送。该文本 fixture 使用持久 predecessor 消息副本，不调用私有 continuation API。

接口依赖：已整合的 `Store::execution_admission_status`、`Engine::new_recovered`、`Engine::shutdown`；没有新增生产公开接口。

## 验证边界

仅执行上述两文件 rustfmt（edition2024、skip_children）和 `git diff --check`，退出码均 0。没有运行 Cargo、编译、测试、Host 或 probe。新 fixture 的真实收束及事务竞争由经理固定 full-3 验证，尚不宣称通过。

只读 external-1 原日志 SHA256：`1e8126d815798003c11d353ef83a0f5b85b81a8e95ddac4ec64c6aab26d60bef`。D 所管 target 结果为 approval_http 11/5、turn_http 16/1、checkpoint_http 3/0、workspace_http 2/0、worktree_http 2/0；后面三文件没有因本轮失败需要改动。原失败记录保留，不能用修复提交改判。
