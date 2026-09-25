# 审批 HTTP 接口预检

日期：2026-09-26。执行人：员工 B（`gpt-5.6-sol`）。任务：`C-R3-PREFLIGHT-B`，只读接口契约核对。固定源码基线：main `379e2aa6378bbe1161c9f65f6655f93876834ac0`；第二轮审批实现已验收，本任务不重开 B-R2。

本次只读检查 C 的[审批 HTTP 契约草案](../../../员工C/任务/第三轮/审批HTTP契约草案.md)与 `Engine` / `Store` / `Repository` / `ApprovalError` / 现有 server 错误映射。未改产品源码、C 档案、测试或 Git 暂存区，未运行产品测试。

## 结论

草案的路由、DTO白名单、鉴权、CAS决定和404/409/500总体方向可实现，但以下三处必须在正式派发前修订或列为明确依赖。

### 1. AH14 不能要求 binding conflict 由 decision 入口触发

真实决定入口为：

- `Engine::decide_approval(approval_id, approved, decided_by)`
- `Store::decide_approval(approval_id, approved, decided_by)`

它只接收审批 ID、布尔决定和决定人，不接收 task、tool call 参数或绑定摘要。Store 读取审批后只检查 `status == pending`，随后 CAS 写 approved/denied；决定阶段不重算绑定。

`ApprovalError::BindingConflict` 的真实触发点是 `ensure_approval`、`claim_approval` 及恢复/执行前的绑定重查。因此正式 HTTP 契约不能声称 `POST /api/approvals/{id}/decision` 可因 binding mismatch 返回409，也不能用该路由造 AH14 binding 反例。篡改导致的审批关系/消息损坏在 `Repository::validate_approval` 中应归 `CorruptState`→500；合法审批在决定后、实际 claim/reconcile 时发现 binding mismatch，则任务执行链失败且零工具副作用，HTTP decision 本身可能已经200。

建议把 AH14 拆为：

- HTTP decision 对已非 pending 的 unknown 记录返回409、`retryable=false`，不新增 resolved/卡片、不执行工具。
- binding mismatch 保留为既有 B 层安全不变量；C 可做只读/集成观察，但不得要求 decision 请求同步返回409。若产品必须让 decision 同步验证绑定，需要新增后端命令/API，由 B 单独实现并重新冻结，不能在 server handler 拼装。

unknown 的真实细节：recover 将 claimed 改为 unknown 时，审批通常保持 `status=approved`。再次 decision 首先因 status 非 pending 返回 `ApprovalError::Conflict`，所以409可实现，但不是 `ApprovalError::UnknownResult`；后者由 resume/执行恢复入口触发。

### 2. task 审批列表必须显式验证 task

`Store::approvals_for_task(task_id)` 直接按 approvals.task_id 查询。未知 task 没有审批行时自然返回空数组，无法区分“存在 task 且无审批”和“未知 task”。因此草案“存在任务空数组”与 AH11“未知 task 404”不能仅靠列表查询同时成立。

C 的 handler 可在读取列表前调用公开 `Store::task(task_id)`：

1. `task()` 成功，再调用 `approvals_for_task()`，空数组即合法200。
2. task 缺行的 `QueryReturnedNoRows` 错误链沿现有 `read_error` 映射404。
3. task JSON/身份损坏和其他读取失败沿 `read_error` 映射安全500。

这不需要新后端 API。正式测试必须分别创建真实存在且零审批的 task，以及完全未知 task，不能只用空 approvals 表证明两者。

### 3. busy/locked 在审批 Repository 查询中会被擦除

现有 server 的 `is_transient_store` 能沿 error chain 识别 `DatabaseBusy` / `DatabaseLocked` 并映射 `500 internal, retryable=true`。但审批 Repository 在下列路径先把底层 rusqlite 错误无条件替换为 `ApprovalError::CorruptState`：

- `approval_by_tool_call`：`.optional().map_err(|_| CorruptState)`。
- `approval`：`.optional().map_err(|_| CorruptState)`。
- `approvals_for_task`：rows collect 的任意错误映射为 `CorruptState`。
- `validate_approval`：task 关系查询和 scope 查询的任意错误映射为 `CorruptState`。

一旦 busy/locked 在这些查询点发生，原 rusqlite 错误已不在 chain 中；C 的 HTTP mapper无法判断 transient，会把它当损坏或普通500，`retryable=true` 无法可靠兑现。事务开始、UPDATE、prepare 等仍有部分路径保留原错误，但不足以满足 AH11 的普遍承诺。

正式任务依赖一个 B/后端小修：仅把“确认的数据形状/关系/JSON不变量错误”转成 `CorruptState`，让 SQLite busy/locked 和其他操作错误保留原 error chain；NotFound 仍必须与损坏区分。修复需覆盖 GET single、GET task list、POST decision 在审批初读/校验点的 busy/locked。C 不应在 server 通过错误文案猜测，也不能自行修改 Repository。

## 可冻结的 HTTP 错误矩阵

在上述 Repository 错误保真依赖完成后，建议正式契约固定为：

| 入口/错误 | HTTP | code | retryable |
| --- | --- | --- | --- |
| approval id `ApprovalError::NotFound` | 404 | `not_found` | false |
| task 列表前置 `Store::task` 缺行 | 404 | `not_found` | false |
| `ApprovalError::Conflict`（含重复/反向决定及 approved+unknown 再决定） | 409 | `conflict` | false |
| `ApprovalError::BindingConflict` / `UnknownResult`（若未来其他审批HTTP命令真实返回） | 409 | `conflict` | false |
| `ApprovalError::CorruptState` | 500 | `internal` | false |
| 保真的 SQLite busy/locked | 500 | `internal` | true |
| 其他存储/序列化/内部错误 | 500 | `internal` | false |
| body/path/content-type/limit/guard | 沿草案400/403/413/415 | 既有安全 code | false |

审批专用 mapper 应先按 `ApprovalError` typed chain 分类，再识别保真的 transient store；对于 typed `CorruptState` 不得因链中偶然残留的其他错误降级或标成可重试。响应只返回固定安全 message，不暴露 ID、SQL 或内部 error debug。

## 对正式任务的建议

1. 先派 B/后端完成审批 Repository 错误保真及定向测试，再冻结 C 完整基线；或者把 AH11 busy/locked 标为明确阻塞，C 先实现其余路由但不能宣称全契约通过。
2. 正式 C 任务删除“decision 触发 binding conflict 409”的要求；unknown decision 409明确绑定 `Conflict` 状态语义。
3. GET task approvals 明确要求 `Store::task` 前置存在性/损坏校验，再查询排序列表。
4. DTO可直接从 `ApprovalRecord` 白名单映射；不得暴露 `args_digest`、`binding_digest`、workspace、write_scopes、allow_commands。状态与执行态使用既有 snake_case serde 值。
5. POST decision 调用 `Engine::decide_approval`，以便提交后通知当前 waiter；handler不得直调 Store 后另造唤醒逻辑，也不得主动 resume/provider。

下一步第一操作：经理修订 C 正式契约，并决定是否先派 B 完成 Repository busy/locked 错误保真依赖。当前没有产品实现写权。

## 正式契约修订1补充（2026-09-26，员工 B）

经理发布的 C-R3-01[正式契约修订1](../../../员工C/任务/第三轮/审批HTTP正式契约.md)已采纳并收窄上述草案预检结论：部分 GET Repository 查询已经擦除 SQLite cause 时，固定返回安全 `500 internal, retryable=false`；只有仍明确保留 `DatabaseBusy` / `DatabaseLocked` cause 的错误链才允许 `retryable=true`。在这个限定下，**全面 GET 错误保真修复不再是 C 本切片的前置或阻断**，上文相关建议保留为草案阶段的历史分析。

AH11 要求的“真实 busy 决定故障”可由现有接口实现。`Store::decide_approval` 的第一项数据库操作是：

```text
db.transaction_with_behavior(TransactionBehavior::Immediate)?
```

该 `?` 没有经过 Repository 的 `CorruptState` 替换。另一个 SQLite 连接持有同一数据库的写事务时，decision 连接在现有 `busy_timeout=5000` 后返回 `rusqlite::Error::SqliteFailure(DatabaseBusy, ...)`；cause 保留在 anyhow chain，审批专用 mapper 可据此返回 `500 internal, retryable=true`。审批行仍为 pending，不能新增 resolved、决定时间或副作用。

建议 C 在临时数据库上使用以下确定性故障注入：

1. 先通过真实 Engine/provider 形成 pending 审批，并记录审批行、resolved事件数和副作用计数。
2. 在独立阻塞线程打开同一个数据库文件的新 `rusqlite::Connection`，启用 foreign keys；执行 `BEGIN IMMEDIATE`（或 `transaction_with_behavior(Immediate)`）并在事务真正成功后通过 channel/Barrier 通知测试主协程。
3. 持锁线程等待另一个 channel，不提前提交或回滚。收到“锁已持有”信号后，主协程才发真实 HTTP decision，避免 sleep 猜时序。
4. 允许应用连接按既有5秒 busy timeout真实失败；HTTP客户端超时必须大于5秒，例如10至12秒。断言500、`code=internal`、`retryable=true`、固定安全 message/error及安全头。
5. HTTP响应完成后通知持锁线程执行 `ROLLBACK` 并 join；最后查询审批仍pending、resolved未增加、文件/命令/provider计数未增加。测试失败清理也必须释放事务，避免污染后续用例。

跨连接写锁在 WAL 下通常返回 `SQLITE_BUSY`，这已满足正式 AH11 的“保留SQLite链的真实busy决定故障”。不必强造 `SQLITE_LOCKED`；后者常与同连接/shared-cache语义相关，使用伪造 anyhow 错误只能作为 mapper 单元测试，不能替代真实 HTTP busy 证据。若用 `BEGIN EXCLUSIVE` 也应先确认事务已获得锁再请求，不以固定 sleep 证明竞争前置条件。

正式契约对 AH14 的修订也与真实API一致：unknown GET 200；approved+unknown 重复决定由 `Conflict` 返回409；pending绑定变化的 decision 可能200，随后执行前 fail closed。BindingConflict/UnknownResult 的 mapper类型测试可保留，但不得声称它们可由当前decision HTTP真实触发。

本补充为只读核对，未运行产品测试、未修改源码或 C 档案。当前 C-R3-01 不依赖 B 后端返工，可按正式契约修订1实施。
