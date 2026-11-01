# NEXT-21 W04 固定候选清理复核（C-R31）

日期：2026-11-01；执行人：员工 C。范围：只读固定 Git blob `773a32c6f32c857fab91732e8980104ade0003cc`，相对 `ba4f72cf`；产品树 `d43923db0cd30993ebc5467bf4be78efbfafced7`、Rust 树 `744f9878f7f39a2e8fb43cdd96bc8ee150e8af38`。main 基线 `b66b2229bee7aeb3e00378e135920eb61c85a481`。不读取在途工作树替代候选。

## 结论

本次指定范围未发现有证据的阻断性生产缺陷或测试伪阳性；静态复核支持进入经理安排的首次动态验证，不能据此声称 W04 已通过。R04 超时或 Windows API 无法确认清理时仍须按 unknown 停止并检查现场；测试兜底调用终止 API 本身不是终结证明。本轮未构建、运行测试或进程，未修改产品。

## 证据与边界

- `src/wasm_worker_process.rs:1280-1316`：先真实等待 event；持续事件注入从真实 `WAIT_TIMEOUT` 起将结果替换为 `WAIT_FAILED`，进入相同观察失败分支。`:1318-1355` 在 event 已报告 signaled 后持续注入未知结果错误，进入与实际 `GetOverlappedResult` 失败同一错误分类。此为受控注入而非自然 OS 错误的实测证明；result 注入时内核操作可能已实际完成，但软件尚无可信完成结果。
- `:1358-1376`：pending 时 Drop 反复 CancelIoEx 和 poll，未知错误不清 pending；pipe、event、OVERLAPPED 和 buffer 随 Operation 保持存活。可信完成或已分类终态错误才退出。持续观察失败允许线程无限保留，这是 fail-closed 可用性代价，不能当作已回收。
- `:414-428, 457-487, 503-545`：SlotLease 移到监督线程，调用方 future Drop 通过 CancelOnDrop 封 gate，不释放线程内 lease；超时返回 CleanupUnknown 不代表槽已释放；线程在 platform::run 返回后才释放槽。Gate 的授权与停止受同一 admission mutex 序列化；已授权调用仍需靠 pending 和收割处理。上述为正常返回与析构路径的静态分析，并非对所有系统故障的动态证明。
- `src/wasm_worker_process_tests.rs:1251-1332`：分别注入两类持续错误，检查同时调用、unknown 返回后调用及重建 service 均 Busy；释放注入前无 stdin settled，释放后等待槽恢复、观察原进程信号和各阶段收割。实际执行结果待 B 窗口；注入 guard 在 panic 时解除，非实际验证结果。
- `src/wasm_worker_process.rs:2045-2201`：R04 实际创建挂起 worker 并放入 real Job，让 ProcessGuard 持另一个 empty Job；验证真实成员关系及同一进程观察句柄。测试另有 2.5 秒 watchdog 终止 real Job，断言 watchdog 未使用，并检查进程信号和 real Job 归零，避免 watchdog 成功造成伪阳性。unwind guard 尝试终止 real Job，且 Job 有 kill-on-close；但 API 失败、查询不可信或 6 秒仍未完成时测试不能证明已经清理，cleanup 线程可能仍保持运行，必须按 unknown 停止、保全并检查，不能按测试 panic 或 PID 猜测为已清理。

经理另审 WAT、IOCP 和其他矩阵；本报告不替代这些审查。下一步等待经理核收和动态窗口，C 释放。
