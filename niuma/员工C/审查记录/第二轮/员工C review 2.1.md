# 员工 C review 2.1：新增 Turn HTTP 补证复审

日期：2026-09-23。审查人：项目经理。任务：C-R2-01 / NEXT-01 修订2.1；执行标识 `C-R2-01-20260923-0701`。关联报告：[员工 C 第2.1轮补证报告](<../../提交报告/第二轮/员工C（新增Turn-HTTP接入 第2.1轮补证报告）.md>)；证据目录：[NEXT-01-2.1证据](../../提交报告/第二轮/NEXT-01-2.1证据/)；前轮审查：[review 2.0](<员工C review 2.0.md>)；补证任务：[修订2.1](../../任务/第二轮/新增Turn-HTTP补证修订2.1.md)。

## 结论

**APPROVED（C-R2-01 / NEXT-01 新增 Turn HTTP 写入口及联合验收范围）**。

整合前置仍按 [双线协调](../../../项目经理/任务/2026-09-23C-D双线协调.md)：本结论不等于 Git 提交、合并或发布，也不覆盖真实插件生命周期、审批、CLI 等未交付能力。经理本轮未重新运行产品测试，结论依据员工证据目录、逐文件哈希核对与全部补证条目的静态代码复核；包含 C 与 D 的联合候选已由经理另跑串行门禁，结果见 [联合门禁记录](../../../项目经理/审查记录/2026-09-23C-D联合门禁.md)。

## 经理复核事实

- `NoManCode/rust-app/src/server.rs`：SHA-256 `d89f035c0fdb8ca8b3e316e73e2d8f80a3e134d535e6a66e743d20f1f9959667`，与 2.0 轮相同，本轮未改生产代码，仍符合契约。
- `NoManCode/rust-app/tests/turn_http.rs`：2717 行，SHA-256 `9b8d53617674c4c06d29aa52690fbfae6b9c8746d5aa37a14a928d9c1f94f954`，与报告一致。
- D 的三处源码与 [D review 1.0](../../../员工D/审查记录/第一轮/员工D%20review%201.0.md) 哈希一致；rust-app 其余文件与 86 文件派发前基线逐文件一致，无范围外改动或缺失。新增文件仅为契约允许的三处与两份既有忽略日志（baseline.log / adversarial.log，已列入基线收尾例外）。
- `WATCHED_TABLES` 为 11 张表（projects/sessions/turns/agents/turn_tasks/turn_task_dependencies/idempotency_records/runs/tasks/idempotency/events），与报告的 11 表口径一致。
- 员工证据：定向 `cargo test --test turn_http` 17/17 两次（含 `--nocapture` 的 n07 实测长度行）、workspace `build.ps1 -Action test` 143 通过 0 失败 1 付费 live 忽略、fmt/clippy/wasm-check/`git diff --check` 退出码 0。以上为员工快照证据，经理未在本轮复跑。

## 逐项复核（review 2.0 条目 → 当前代码 → 结论）

| 条目 | 复核要点 | 结论 |
| --- | --- | --- |
| P1·N07 阈值 | `candidate_len` 从前序 task 的 `value.messages` 重建并实测 `serde_json::to_vec` 长度；over 注入断言 `>1_500_000`（实测 1500001），boundary 注入断言 `<=1_500_000`（实测 1500000），nocapture 日志行与代码 `eprintln` 对应。超限请求在注入后、请求前取 11 表快照与调用计数，400/request_failed/false 且零副作用；恢复原文后 boundary 201 + 同 key 回放 200 | 关闭 |
| P1·N09 前置快照 | 三个 404 与 foreign_agent/foreign_turn/team/双 Agent 全部改为请求前 `snapshot`+`calls`，断言 400/404 后 `assert_untouched` 且调用差值 0；team 分支在 `spawn_chat` 落定后另取基线，不再沿用旧计数 | 关闭 |
| P1·N09 故障隔离 | 保存 predecessor task `value` 原文，每例先 `UPDATE ... SET value=原文` 恢复同一合法基线再注入单一故障（tools=true / role=tool / tool_calls 非空数组 / tool_calls 为字符串），各自 400 且零副作用；正例在恢复后的清洁基线上仅注入 `tool_calls=[]` → 201，证明基线本身可追加、每个 400 归因本次注入 | 关闭 |
| P1·N10 前置证据 | stale/running/behind 三分支补齐请求前快照与 `calls_before`；running 保持 `hold:`+`wait_delta(partial-busy)` 真实忙态前置，`release.notify_waiters()` 仍在全部断言之后；cancelled/interrupted 用独立 fresh fixture 且快照在注入后、请求前 | 关闭 |
| P1·N12 竞争记账 | barrier 前取快照+`max_seq`+调用计数；恰一胜者：`turns/turn_tasks/tasks/idempotency/idempotency_records` 各 +1 且旧行前缀不变；`seq>base` 新事件数 == 归属胜者 turn_id+task_id 的事件数且 >0；胜者 `calls_for`=1、败者 0、总差值 +1。失败 key 以最新已完成 turn 重试：独立前后快照各 +1 组、新 turn、该消息调用 1 次，竞争批次账目不被重试掩盖 | 关闭 |
| P1·N14 配置故障 | missing route / missing secret / 不可达 URL 三个新 key 请求各自独立前置快照+调用计数 → 500/internal/false、`assert_untouched`、调用差值 0、逐例恢复；回放断言使用已提交的 `cfg` key（非失败的新 key），route 删除期间仍 200/replayed=true/同 turn，验证回放优先于配置检查；恢复后新 key 201 | 关闭 |
| P2·N11 调度等待 | 单 Engine 与双 Engine+Barrier 两段并发均在断言 `calls_for` 前先 `wait_delta` 观察持久化 delta、`wait_task` 等 completed；两例 1×201+1×200、同 Turn/Task、五表各 +1、queued 事件恰一条 | 关闭 |
| P2·N16 重启快照 | recover+重开 HTTP 后取 `replay_before`/`replay_calls`，断言重启本身零 Provider 调用；回放 200/replayed=true/同 turn（status=interrupted）且 11 表不变；中断前序追加 409/conflict/false 有独立前后快照；resume 后追加 201 链路保留 | 关闭 |
| P2·N05 `%FF`/`%20` | 构造与发送必须成功（build/execute 失败会 panic 显形），服务端 400/request_failed/false 为实证；`%FF` 经 `Path<String>` 非 UTF-8 拒绝、`%20` 经 `validate_persisted_id` 拒绝 | 关闭（服务端证据成立） |
| P2·N06 非 ASCII | `HeaderValue::from_bytes` 必须成功且请求必须发出，`kéy`（0xC3 0xA9）与裸 `0x80` 到达服务端后被 `required_idempotency_key` 的 `to_str` 拒绝 → 400 | 关闭（服务端证据成立） |
| P2·N03 转义 | 字节级核对确认 raw body 实际写入单反斜杠 `\u0068`（`h` 的等价转义）且字段换序；回放 200/replayed=true/同 turn，证明请求哈希按解析后语义计算 | 关闭 |
| 文档项 | 报告正文已将 Git 的 `pwsh.exe` 机器绝对路径改为「Git 内嵌 pwsh shim」相对描述 | 关闭 |

## 保留项（P2，不阻挡放行）

1. 证据目录 `NEXT-01-2.1证据/*.txt` 的 cargo 编译行含本机绝对路径（工具原始输出），与 D 证据同类；按路径约定原始日志保留事实，归档/发布副本正文继续使用相对描述。
2. n05 的 `positives` 数组声明后未逐项复用（`let _ = positives`），三个正例实际逐一发送并断言 404；仅为测试可读性小瑕疵，不影响证据有效性。

## 放行边界

本结论接受：POST `/api/sessions/{id}/turns` 新路由、严格三字段 body、必填单值 Idempotency-Key、新建 201/回放 200、typed 错误映射（含安全 500）、以及 17 组联合验收证据（真实 HTTP、SSE 归属、Provider 增量、11 表副作用、竞争、损坏反例、断线与 partial 重启）。员工报告的 143 通过与经理联合门禁结果分别记录，不相加、不冒充产品总验收。

下一步：经理固定 C/D 联合候选串行门禁（已完成，见上方记录），随后按 [GIT-BASELINE 收尾任务](../../../项目经理/任务/2026-09-23GIT-BASELINE收尾.md) 清点完整基线窗口；独立工作树派生仍以基线登记为前提。
