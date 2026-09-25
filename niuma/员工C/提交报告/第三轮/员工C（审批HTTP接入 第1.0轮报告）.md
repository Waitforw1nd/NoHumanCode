# 员工C：审批HTTP接入第1.0轮报告

日期：2026-09-26；任务 `C-R3-01 / NEXT-02D`；执行标识 `C-R3-01-20260926-014759`；实际模型 `gpt-5.6-sol`。

开发基线 `379e2aa6378bbe1161c9f65f6655f93876834ac0`，分支 `codex/c/approval-http`，最终固定候选 `00a4c2bd675516ca61fac84c03734052ccb03660`。首个实现提交为 `2af377026f83c0b8b3c6f24cf5a268bac07090ee`，审查补证提交为 `00a4c2bd675516ca61fac84c03734052ccb03660`。候选相对基线只修改 `NoManCode/rust-app/src/server.rs`，并新增 `NoManCode/rust-app/tests/approval_http.rs`。

## 交付行为

- 新增 `GET /api/tasks/{id}/approvals`、`GET /api/approvals/{id}`、`POST /api/approvals/{id}/decision`。列表先确认任务存在；决定只调用冻结的 `Engine::decide_approval`，`decided_by` 固定为 `user`，不启动 worker。
- 响应使用显式白名单 DTO：审批和执行状态、归属、工具名、安全 preview、时间及决定人。没有序列化持久记录中的 digest、workspace、权限、命令或原参数。
- POST body 只接受严格的 approve/deny；沿用 1 MiB、Content-Type、Host/Origin/Sec-Fetch-Site、写 token、安全头规则。路径 ID 经持久 ID 校验。
- 审批类型链分别映射 404/409/500；busy/locked 才可重试。没有复制绑定校验、claim、resume、执行或 SQL。

源码 SHA-256：`server.rs` 为 `D5BAC22AA25585619CDFAF17F6B164BC3A2FBD1BD6BB6658DDBDDFC4303A20EB`；`approval_http.rs` 为 `F1DB104509BF7DBC60DAAE9C56812AD007E196D14E09528AE24DD9628316413F`。

## AH01-AH14 证据

| 验收项 | 最终证据 |
| --- | --- |
| AH01-AH04 | 真实 pending 列表/单卡/空列表；批准 write_file 与 PowerShell 各执行一次，拒绝零副作用；读取工具无审批。 |
| AH05-AH06 | 真实 Runtime shutdown/reopen/recover；无 worker 的 HTTP 决定不执行，显式 resume 一次；denied 不执行，finished 不重做，claim 后真实副作用恢复 unknown 且不重试。 |
| AH07-AH08 | cancel/decision 与八路 approve/deny 使用 Barrier 同步竞争；持久决定和 resolved 均恰一次；claim 先发生时不抹除真实副作用。 |
| AH09 | DTO 精确字段集；真实 `approval.requested` 与 `approval.resolved` 分类型精确字段集，并扫描序列化事件不含 mock secret、正文键或 `arguments`；preview 长度和脱敏保持 B 契约。 |
| AH10 | 两个真实任务复用同一 tool_call_id，各自决定且列表隔离。 |
| AH11 | malformed/重复/未知字段、类型、Content-Type、413、路径、token、三类 guard、404、坏任务、坏审批和真实 busy；逐请求校验精确 status/code/message、`error == message`、retryable、安全头及无敏感词。413 和三类 guard 均在 fresh pending 卡上逐次证明状态、事件总数、resolved、工具事件和文件副作用不变。 |
| AH12-AH14 | 无等待者决定仅持久化；Run/Session SSE 实际断线和 cursor/header 续传；unknown 可读且重复决定冲突；绑定变化在决定后由执行前校验 fail closed。 |

定向测试 `cargo test -p peachsh --locked --test approval_http -- --nocapture`：14 通过、0 失败，2026-09-26 02:27:31 至 02:27:38（+08:00），退出 0。原始日志及元数据位于 [C-R3-01证据](C-R3-01证据/)。审查要求的 AH09/AH11 补证已包含在最终 SHA。

## 最终门禁

固定 SHA `00a4c2b` 上使用独立 `CARGO_TARGET_DIR`，在同一 PowerShell 进程初始化现有 MSVC 环境后执行：

| 命令 | 结果 |
| --- | --- |
| `build.ps1 -Action check` | 退出 0 |
| `build.ps1 -Action test` | 退出 0；222 通过、0 失败、1 个付费 live 忽略 |
| `build.ps1 -Action fmt` | 退出 0；空输出日志已保留 |
| `build.ps1 -Action clippy` | 退出 0 |
| `build.ps1 -Action wasm-check` | 退出 0 |

每项均有独立 `*-00a4c2b.log` 与 `*.meta.txt`。此前直接 cargo 进程未初始化 MSVC 而失败的日志继续保留，未改写为通过；未执行付费 live 或 release build。

## 审查状态与限制

经理 review 1.0 对首候选判定 `CHANGES_REQUIRED`，范围仅为 AH09/AH11 证据补强；最终补证提交后尚待 A 差异复核和经理 review 1.1。员工不宣告验收、整合或发布。应用层既有语义仍是：HTTP 决定本身不做绑定重查；活跃 waiter 可在批准后继续；claim 后崩溃恢复 unknown，不自动重做。

## 作者补充：AH01-AH14逐项索引与边界

以下索引对应最终候选 `00a4c2b` 的 `tests/approval_http.rs`；组合测试名不代表合并验收项，各项关键断言分别如下。

| 项目 | 测试函数与关键断言 |
| --- | --- |
| AH01 | `ah01_ah09_ah10_safe_dto_order_and_task_isolation`：真实 pending 的列表和单卡返回安全 DTO，顺序等于 Store `(created_at,id)` 顺序；存在任务无卡返回空数组，审批前没有工具副作用。 |
| AH02 | `ah02_ah03_ah04_real_tools_follow_http_decisions`：HTTP approve 后真实 `write_file` 仅执行一次；deny 不写文件并产生安全 tool error。`ah02_ah08_ah12_decision_snapshot_conflict_and_concurrency` 另断言决定响应快照为 approved/not_started、重复或反向决定 409、resolved 不增长。 |
| AH03 | `ah02_ah03_ah04_real_tools_follow_http_decisions`：真实读取工具直接执行并返回结果，审批列表为空且没有 `approval.requested`。 |
| AH04 | `ah02_ah03_ah04_real_tools_follow_http_decisions`：真实 PowerShell 计数文件批准后恰一行，拒绝路径为零；测试要求任务真实完成，不把 spawn 失败当作拒绝成功。 |
| AH05 | `ah05_shutdown_reopen_http_decision_without_worker_then_resume_once`：真正 shutdown 旧 Runtime 后重开 Store/Engine 并 recover；无 worker 时 HTTP approve 不增加 provider/文件调用，显式 resume 后只执行一次，再次重开 finished 不重做。 |
| AH06 | `ah06_denied_http_decision_survives_real_runtime_restart_without_execution`：deny 跨真实 Runtime 重启保持不执行。`ah06_real_command_side_effect_then_shutdown_recovers_unknown`：命令真实产生一次副作用、结果提交前 shutdown，recover 后为 unknown；GET 200、重复决定 409、resume 失败，计数仍为一次且没有 tool_result。AH05 的第二次重开同时覆盖 finished 不重做。 |
| AH07 | `ah07_cancel_before_claim_and_after_real_command_claim`：取消先发生时卡为 cancelled、零工具启动；claim 先发生时保留已经发生的真实命令副作用并阻止下一调用。`ah07_barrier_cancel_and_http_decision_have_one_durable_resolution`：Barrier 同步 cancel/decision 竞争，只允许一个成功结果和一条 resolved。 |
| AH08 | `ah02_ah08_ah12_decision_snapshot_conflict_and_concurrency`：Barrier 同步八路 approve/deny，恰一条 200、七条 409，持久决定一次且 resolved 仅增加一次。 |
| AH09 | `ah01_ah09_ah10_safe_dto_order_and_task_isolation`：DTO 键集合精确等于契约白名单，preview 字符上限成立且响应无 mock secret/digest/workspace/权限字段；真实 `approval.requested`、`approval.resolved` 按类型核对精确 data 键集合，并扫描序列化事件无 mock secret、正文键或 `arguments`。 |
| AH10 | `ah01_ah09_ah10_safe_dto_order_and_task_isolation`：两个任务相同 call id 的列表和决定隔离。`ah10_real_workers_with_same_call_id_do_not_borrow_decisions`：两个真实 worker 各自等待和执行，只接受本任务审批决定。 |
| AH11 | `ah11_strict_inputs_guards_paths_and_safe_errors`：malformed、重复/未知字段、错类型、缺 Content-Type、413、非法路径、token、Host/Origin/Sec-Fetch-Site、两类404和坏任务500均走真实 HTTP；逐请求断言精确 status/code/message、`error == message`、retryable 和安全头。413及三类 guard 每次后均保持 pending、事件总数不变、无 resolved/tool_start/tool_result/文件副作用。`ah11_real_busy_decision_is_retryable_and_corruption_is_not`：真实 `BEGIN IMMEDIATE` 决定故障为500/retryable true，损坏审批为500/false。 |
| AH12 | `ah02_ah08_ah12_decision_snapshot_conflict_and_concurrency`：无等待者 approve 只持久化 not_started，不写文件、不启动 provider/worker；重复决定409且无新 resolved。AH05 另以重启后无 worker 场景验证相同边界。 |
| AH13 | `ah13_run_and_session_sse_resume_from_last_consumed_cursor`：Run 与 Session SSE 实际读取 requested 帧后断开；决定后分别用 query cursor 与 `Last-Event-ID` 重连，只读到后续 resolved，断言 seq/cursor、task/session/turn 归属一致且工具最终执行一次。 |
| AH14 | `ah14_unknown_is_visible_and_decision_remains_conflict`：unknown GET 返回200实际状态，重复决定409且无新 resolved。`ah14_http_decision_does_not_claim_binding_validation`：篡改 binding 后 HTTP 决定可200，但执行前既有绑定校验 fail closed，任务失败且无工具事件/文件副作用。 |

错误边界需按可达性理解：Repository 的部分读取路径会把 SQLite cause 擦除成 `CorruptState`，因此这些 GET 只能安全返回500且 `retryable=false`，本报告不把它们写成可识别 busy 的 GET。`BindingConflict` 与 `UnknownResult` 的 mapper 分类由 `server::tests::approval_error_maps_typed_variants_through_context_and_source` 覆盖；`CorruptState` 和保留 cause 的 busy 分类由 `server::tests::approval_error_keeps_corruption_500_and_busy_retryable` 覆盖。这两种冲突 mapper 不声称能由本轮 HTTP decision 路径直接触发。

A 的第1.1轮差异复核已确认 `C-R3-EVIDENCE-01 / AH09` 与 `C-R3-EVIDENCE-02 / AH11` 两项缺口闭合；经理最终 review 尚待落档。固定候选继续冻结，员工不据此宣告验收、整合或发布。

作者更正（AH07）：上表“只允许一个成功结果”不表示 cancel 与 decision 两个 HTTP 响应必须恰一条 200；批准先胜时，decision 可返回200，随后 cancel 也可返回200并取消后续执行。测试约束的唯一性是审批持久决定只发生一次、`approval.resolved` 恰一条，且已经发生的 claim/外部副作用不被抹除。
