# NEXT-01：新增 Turn HTTP 接入契约

日期：2026-09-23。修订：1。项目：NoHumanCode；仓库根 `./`，源码 `NoManCode/`，Rust 工作目录 `NoManCode/rust-app`。制定人：项目经理。

**状态：已派发、执行中，尚未提交本轮报告或验收。** 来源为用户直接交付提示词及本轮明确确认“C在执行任务”；C身份已登记 `C-R2-01-20260923-0701`。第一轮已结束，本任务为新切片C-R2-01，对应全局NEXT-01；不另派第二位执行者。

## 1. 目标与依据

为已有、单 Agent、无工具历史且上一轮已完成的 Chat Session 增加一条真实 HTTP 追加链：HTTP → `Engine::send_chat_turn` → Store 原子追加 → 后台执行 → 同 Session 查询与 SSE。一次新消息产生独立 Turn/Task；重复请求返回该轮对象且不再次派发。此处完成的是后端传输接入，不等于整个连续聊天产品流程完成。

依赖：[B 最终契约](<../../../员工B/审查记录/第一轮/员工B 最终审查.md>)、[C 最终审查](<../../审查记录/第一轮/员工C 最终审查.md>)、[现有 HTTP/SSE 契约](<../第一轮/员工C HTTP-SSE契约.md>)。源码核对入口：`domain.rs` 的 SendChatTurn/ChatTurnReceipt/ChatTurnError，`engine.rs` 的 send_chat_turn，`store.rs` 的 append_chat_turn，`server.rs` 的 router/guard/ContractJson。

## 2. Git 基线与唯一写入人

| 项目 | 本任务安排 |
| --- | --- |
| 模式 / 工作树 / 分支 | 共享工作树过渡模式；`./`；当前 `main`。本任务不从旧 HEAD 创建独立工作树 |
| HEAD | `8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`，只表示底层旧提交，不是完整实现基线 |
| 有效基线 | HEAD 加全部继承的修复、迁移和维护；[本轮工作区清单](../../../项目经理/审查记录/2026-09-23NEXT-01工作区基线.json) 记录 Git 状态及 86 个非忽略源码目录文件的 SHA-256/长度 |
| 暂存 / 提交 / 分支操作执行人 | 项目经理；C 只读取 Git 状态、编辑授权文件、提交报告 |
| 整合人 | 项目经理；首次完整 Git 快照与新功能验收分别记录 |
| 当前执行者 | 员工C，执行标识C-R2-01-20260923-0701，已由本人身份登记；原Grok 4.7安排保持 |

开工比对清单与当前差异。已登记的经理文档更新不影响源码基线；发现额外源码变化时先识别来源，不覆盖。若经理先完成 GIT-BASELINE 并改用独立工作树，必须发布本任务修订2，填写真实完整 SHA、实际分支/路径和提交执行人；不能自行套用预想分支或只检出旧 HEAD。共享模式可按本修订执行，完整 Git 基线仍须后续收尾。

唯一写入范围：

| 文件 / 资料 | 写入人 / 边界 |
| --- | --- |
| `NoManCode/rust-app/src/server.rs` | C：新增路由、局部 DTO、严格幂等头解析、专用错误映射，以及必要的本模块映射单测 |
| `NoManCode/rust-app/tests/turn_http.rs` | C：新增真实 HTTP 联合验收。测试夹具可操作临时数据库，生产 handler 不得自行拼 SQL |
| `niuma/员工C/提交报告/第二轮/`、C 个人身份 | C：实现记录、有限脱敏附件和实际执行状态 |
| C 第二轮任务、验收矩阵、C 审查记录、全局管理资料 | 经理：修订范围、审查与验收 |

本修订不授权修改既有 `tests/http_contract.rs` / `tests/session_turns.rs`、domain、Engine、Store、Repository、secrets、protocol、Cargo/锁文件、构建脚本、UI 或 schema。已有能力足够做适配；若发现不可绕过的依赖，提交具体入口、有效反例、建议范围，经理修订任务后再改。A/B 本轮没有支持子任务或写入授权；其既有契约是依赖，经理负责跨层审查。D另有独立插件目录任务包，见下方并行登记，不改C范围。

## 3. 路由与请求

新增 `POST /api/sessions/{id}/turns`，在现有 GET route 上增加 POST，保留 GET 行为。成功使用 `application/json`；写鉴权、安全头、1 MiB body 限制沿用 guard/ContractJson，不增加另一套 CORS 或鉴权。

请求示例中的 ID 是占位值，调用方应使用真实查询结果：

```http
POST /api/sessions/<session-id>/turns
Content-Type: application/json
x-peachsh-token: <当前本机写令牌>
Idempotency-Key: turn-002

{"agent_id":"<agent-id>","expected_last_turn_id":"<turn-id>","message":"继续分析这个实现"}
```

body 为严格对象，三个字段均为必填字符串；拒绝 null、错误类型、缺失字段、重复字段及未知字段（局部 DTO 使用 `deny_unknown_fields` 或等价约束）。因此 body 中的 `session_id`、`idempotency_key`、route/model/tools 等均为 400。Session ID 唯一来自 path；不从 body/query 推断或覆盖。字段顺序、JSON 空白与等价转义不改变解析后的命令。

- path 解码失败通过 `PathRejection`/`object_id` 转为结构化 400。
- session_id、agent_id、expected_last_turn_id 复用 `secrets::validate_persisted_id`：1～128 个 UTF-8 字节、无空白/控制字符、拒绝现有已知敏感标识。不是 UUID-only，不更改大小写或 trim。只作格式校验，不在 HTTP 层先查存在性/归属/latest。
- message 原文传入 Engine。trim 仅用于判空，不修改字符串；非空白且最多 100,000 UTF-8 字节。累计 messages JSON 的 1,500,000 字节约束仍由应用层负责，HTTP 不读前缀或另行压缩。
- 首轮创建继续使用既有 `POST /api/runs`；本接口不创建空 Session，不使用 resume 冒充新 Turn。

### Idempotency-Key

新接口必须恰好一个 `Idempotency-Key`。使用 `HeaderMap::get_all` 检查单值，缺失、两个及以上（即使相同）、非法文本均返回 400。旧可选 `idempotency_key()` 只取第一值，不能直接复用为严格解析器，也不改变旧 Run 行为。

值为 1～200 个可见 ASCII 字节，复用 `validate_idempotency_key` 的敏感值拒绝；不 trim、不 URL 解码。**新接口另拒绝逗号**，使代理合并后的 `key-a,key-b` 无法被当作单 key；这是本接口的显式收紧，不回溯改变旧 Run 的可选 key 契约。body/query 不能补足缺失 header。

key 沿用 Store 的既有全局命名空间，不按 URL/Session 再加前缀。稳定摘要由应用层生成，包含命令版本 `engine.send_chat_turn.v1`、三个身份和原始 message；不包含 key、时钟、生成 ID 或运行状态。不同 Session/Agent/前序/message 共用 key 均冲突，既有 Run 使用过的 key 也不能拿来创建新 Turn。

格式合法后仅调用一次 `Engine::send_chat_turn`。**回放/冲突判断在资格、最新前序与路由检查之前**，传输层不能先加载 Session/Agent/前序或调用 key()。同 key 同请求在后台运行中、完成后、出现更后轮次或重启后仍按应用结果回放；不自动 resume/launch。

## 4. 成功响应

| 结果 | HTTP | JSON |
| --- | --- | --- |
| 新事务已提交 | 201 Created | `{"turn": <Turn>, "task": <TurnTask>, "replayed": false}` |
| 已有相同命令 | 200 OK | `{"turn": <Turn>, "task": <TurnTask>, "replayed": true}` |

三个顶层字段必须存在。嵌套对象直接使用应用回执中已有可序列化的 Turn 和 TurnTask，保留现有字段；局部 response DTO 或 `json!` 即可，不给 domain 添加 serde derive。`task` 不是带 messages/route/output 的 legacy Task，不返回 legacy Run，也不从 Run 首任务猜 ID。

必须满足 `task.turn_id == turn.id`，两者 session_id 等于 path，task.agent_id 等于请求身份，legacy_task_id 是该轮兼容任务。首次回执是提交时快照，通常 queued，不表示 Provider 已完成；回放保留原身份，但 status/updated_at 允许体现回放时持久状态，不要求 JSON 逐字等同第一次。观察后续状态使用既有查询/SSE。

不增加必需的 Location、业务事件类型或 schema/protocol 字段。提交后客户端丢失响应时可以用同 key 重试；SSE 断开仅结束订阅，不能撤销命令。已提交未 launch 或 partial 重启沿用 interrupted；同 key 回放不得重派，显式恢复仍走既有 resume。

## 5. 错误映射及次序

沿用 `{code,message,retryable,error}`，其中 error 与 message 相同。分类查错误链中的稳定类型，禁止解析中文或任意 Display 文本。新增入口专用映射，不能使用默认 `ApiError::from` 把所有错误变成 400，也不能整包调用 `read_error` 将内部缺行误归 404。

| 来源 / 情形 | HTTP | code | retryable |
| --- | --- | --- | --- |
| guard 的 Host/Origin/cross-site/写 token 拒绝 | 403 | forbidden | false |
| JSON、path、ID/key 格式、字段规则失败 | 400 | request_failed | false |
| 缺少/不支持 Content-Type | 415 | request_failed | false |
| body 超过 1 MiB | 413 | request_failed | false |
| `IdempotencyConflict` | 409 | conflict | false |
| `ChatTurnError::InvalidInput` | 400 | request_failed | false |
| `ChatTurnError::NotFound` | 404 | not_found | false |
| `ChatTurnError::UnsupportedSession` | 400 | request_failed | false |
| `StalePredecessor` / `SessionBusy` / `PredecessorChanged` | 409 | conflict | false |
| `ChatTurnError::CorruptState` | 500 | internal | false |
| 剩余错误链含 SQLite busy/locked | 500 | internal | true |
| 其他未分类错误，包括原始 QueryReturnedNoRows、配置/路由/凭据普通 anyhow | 500 | internal | false |

映射顺序：显式幂等冲突 → 显式 ChatTurnError（穷尽枚举）→ 未分类存储瞬态 → 安全默认 500。只以 typed NotFound 对外给 404。CorruptState 即使有底层 cause 也不能降级为不存在或可重试。

新入口所有错误使用本地固定安全说明；无须把底层 anyhow 放进 ApiError。500 例如“连续对话请求处理失败”，瞬态可为“存储暂时不可用”。不回显 SQL、文件路径、请求消息、key、路由名或凭据；保留既有 scrub。409 的 retryable=false 表示不鼓励原样盲重试：调用方应读取当前状态，决定等待、修正前序或使用新 key；同 key 原请求回放仍是正常安全操作。

`Engine::key` 的缺配置/路由被删除/地址变化/凭据不可用目前无稳定业务错误类型。此修订选择安全 500，不增 B 任务、不用中文识别为 400/503。后续若需可操作的细分类，再由经理派发应用类型修订。

guard 先执行；请求存在多个格式错误时不要求跨 extractor 的特定优先级，但每个单故障必须命中对应状态，且所有拒绝都没有该命令造成的写入或 Provider 调用。格式合法后保持 Engine 内的幂等优先级。

## 6. 验收、交付与兼容

逐项执行 [联合验收矩阵](联合验收矩阵.md)，记录测试名、前置条件、实际结果、退出码和证据，不能只写“覆盖”。产品实现、调试和正式测试由 Grok 4.7 C 完成，经理只做契约审查与必要复核。

实现不改变 schema、事件协议、旧 Run/Task 接口、UI、工具/Team/审批范围。保持旧 99 项所在测试集合的有效断言，新增测试后按实际总数记录，不能固定声称 99。不启用付费 live。

报告目标：`niuma/员工C/提交报告/第二轮/员工C（新增Turn-HTTP接入 第2.0轮报告）.md`；附件放同目录 `NEXT-01证据/`。经理 review 目标：`niuma/员工C/审查记录/第二轮/员工C review 2.0.md`，尚未存在即表示未审查。

报告同时写基线清单、实际改动、最终文件 SHA-256、测试时刻、是否 Git 提交/经理验收/合并/发布；共享模式如实写工作区候选。发现应用层问题要给真实反例和对应代码入口，不为使 HTTP 测试通过擅自改底层。完成后刷新 C 身份并交回报告路径。

修订记录：修订1由经理基于当前源码和 A/B/C 有效最终审查制定；只增加新 HTTP 切片，不重新打开第一轮。

派发/并行登记（2026-09-23，经理）：用户确认C执行中，本任务技术契约与写入范围不变。D-R1-01 / NEXT-02A使用用户指定SWE2max，待用户分发，只写新plugin_catalog模块/测试及lib.rs一行导出；[双线协调](../../../项目经理/任务/2026-09-23C-D双线协调.md)明确固定验证快照、独立target和串行整合。C不得覆盖D授权范围，D不得修改C的server/turn_http；本登记不将C任务切换为独立工作树或启动第二执行者。
