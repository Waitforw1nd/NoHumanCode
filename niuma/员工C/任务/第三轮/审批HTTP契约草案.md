# C-R3-01 / NEXT-02D：审批 HTTP 契约草案

2026-09-25，经理助手起草。**未派发、无实现写权。** 依据 [B修订2](../../../员工B/任务/第二轮/审批网关契约补充修订2.md)、[现有HTTP-SSE](../第一轮/员工C%20HTTP-SSE契约.md)。经理管契约/整合，用户转交，Grok员工实现。

## 1. 基线与所有权

main `d8cd41c15b6f880b1b277ad58f9111257bde2ac0` 无审批，仅观察，非C开发基线。B-R2-01既有执行者 `B-R2-01-20260925-0232` 在 `codex/b/approval`、基线 `40da4903ab603d805bc1671a148b39aa86b0b7fc` 实现中；修订2待转交。

B安全实现review通过、整合/API冻结后才填写C完整开发基线并创建独立工作树。建议分支 `codex/c/approval-http`、工作树 `../nhc-c-approval-http/`，均未创建；提交人C，整合人经理。

拟唯一写 `NoManCode/rust-app/src/server.rs` 和新 `NoManCode/rust-app/tests/approval_http.rs`；其他B文件全只读，范围外报阻塞。公开API精确签名/类型尚未冻结，不捏造；开工前登记真实签名及DTO、typed错误映射。

## 2. 路由与DTO

| 路由 | 拟200响应 |
| --- | --- |
| `GET /api/tasks/{task_id}/approvals` | `{"approvals":[...]}`，全部记录按 `(created_at,id)` 排序；存在任务无记录为空数组 |
| `GET /api/approvals/{approval_id}` | 安全DTO |
| `POST /api/approvals/{approval_id}/decision` | 提交后安全DTO |

body仅 `{"decision":"approve"}` / `{"decision":"deny"}`；启用 `deny_unknown_fields`，缺字段/错类型/非法枚举均400。`decided_by` 固定 `user`，不收客户端身份。

显式DTO白名单：id/task_id/session_id/turn_id/tool_call_id/tool_name/status/execution_state/preview/created_at/decided_at/decided_by；类型/可空性待冻结。status沿B；execution_state明确为 not_started/claimed/finished/unknown/cancelled，不从status推算。禁止原样序列化底层行、原参、密钥；preview只用B安全摘要，不补全秘密。

沿现有Host/Origin/sec-fetch-site guard，POST校验 `x-peachsh-token`；1 MiB limit，不放宽CORS。保留安全头、scrub及错误体 `code/message/retryable/error`，error同安全message。

## 3. 错误与执行

| HTTP | code / 情形 |
| --- | --- |
| 400 | request_failed：路径/JSON/body非法 |
| 403 | forbidden：guard/token拒绝 |
| 404 | not_found：typed确认未知task/approval id |
| 409 | conflict：已决定、绑定冲突、unknown操作拒绝 |
| 413 | request_failed：超限 |
| 415 | request_failed：缺失/不支持JSON Content-Type |
| 500 | internal：关系/数据损坏及其他内部失败；busy/locked亦500 |

仅busy/locked的retryable=true。沿error chain typed分类，损坏优先；禁止全部anyhow→400/缺行→404，存储未知失败500。GET unknown仍200；unknown操作/绑定冲突409不可重试，零执行/新卡/新resolved，安全消息。

决定CAS pending→approved/denied与resolved同事务，提交后通知。重复/反向决定409无重复事件。handler不直接resume/execute/provider；批准可唤醒现存等待者合法执行，无等待者只持久化。resume重查绑定/取消/执行状态；unknown不重放、finished不重做、cancelled不恢复；cancel不保证撤销副作用。

原Run/Session SSE承载requested/resolved，不新建路由；kind待冻结。沿既有envelope/归属/seq/游标/错误规则；断线不取消任务。

## 4. AH01–14真实HTTP矩阵

真实端口请求、隔离库/工作区、本地provider；查响应、持久行/事件及副作用，不仅直调handler。

| 编号 | 断言 |
| --- | --- |
| AH01 | pending可查，零写文件/备份/execute |
| AH02 | approve执行一次，deny零执行；resolved一致 |
| AH03 | read直通，无审批 |
| AH04 | 真实命令计数：批准前0/后1，拒绝0 |
| AH05 | 真停止runtime/子进程再重开pending，HTTP approve+resume一次；drop Arc无效 |
| AH06 | denied重启不执行；unknown不重试、finished不重做 |
| AH07 | cancel竞争：取消先于claim零执行，一次resolved |
| AH08 | 重复/反向并发：一成功，其余409，无重复事件 |
| AH09 | HTTP/SSE/存储无秘密、原参、正文 |
| AH10 | 跨task同callid隔离，不借权 |
| AH11 | 输入/鉴权/故障零写；未知id404、损坏500、busy可重试 |
| AH12 | 无等待者批准不启动/执行，仅持久化 |
| AH13 | SSE归属、游标续传不重放，断线不取消 |
| AH14 | unknown/绑定冲突409不可重试，零新卡/resolved |

## 5. 门禁与状态

获批后Rust目录记录 `$PSVersionTable.PSVersion`，按既有 `build.ps1` 分别执行 `-Action check/test/fmt/clippy/wasm-check`（五次），加 `cargo test --test approval_http`。受阻先定位环境，记录实际PS版本/失败日志/缺口；PS5.1 shim不冒充7，不静默替代。不发布build/付费live。

**门禁/产品测试未运行**、PS未检测。阻塞：B待review/整合、API待冻结、C未派发。列SHA、API、AH证据及恢复限制。
