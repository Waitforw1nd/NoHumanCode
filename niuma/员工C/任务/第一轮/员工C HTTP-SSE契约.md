# 员工 C：现有 HTTP 与 SSE 传输契约

日期：2026-09-23。本文描述当前 `NoManCode/rust-app/src/server.rs` 的实际行为，不是后续 Turn 写入口的设计。

## 1. 范围

已收口的是现有读取、既有 JSON 写入口的传输错误、安全拒绝和 SSE。没有挂载 `POST /api/sessions/{id}/turns`，也没有引用未验收的连续回合命令。

成功读取仍返回原有 JSON：

```text
GET /api/health
GET /api/projects
GET /api/sessions/{id}
GET /api/sessions/{id}/turns
GET /api/turns/{id}
GET /api/runs
GET /api/runs/{id}
GET /api/sessions/{id}/events
GET /api/runs/{id}/events
```

Session 与 legacy Run 的映射仍读取持久化的 `legacy_run_id`。分页、列表排序和事件 envelope 没有改。

## 2. 错误体

沿用 protocol 的 `ErrorBody`，并保留兼容字段 `error`：

```json
{"code":"request_failed","message":"可理解的安全错误说明","retryable":false,"error":"同一安全说明"}
```

`code` 使用 snake_case。消息经过既有 `secrets::scrub`，不回显原始请求体、原始 SQL 或内部路径。

| 情形 | HTTP | code | retryable |
| --- | --- | --- | --- |
| JSON 语法错误、字段类型错误、非法游标、非法幂等 key | 400 | request_failed | false |
| JSON 接口缺少或不支持 `Content-Type` | 415 | request_failed | false |
| 请求体超过既有 1 MiB 限制 | 413 | request_failed | false |
| Host、Origin、`sec-fetch-site: cross-site`、写 token 拒绝 | 403 | forbidden | false |
| 查询对象确实不存在，错误链含 `rusqlite::Error::QueryReturnedNoRows` | 404 | not_found | false |
| `IdempotencyConflict` | 409 | conflict | false |
| 读取时的数据损坏 | 500 | internal | false |
| 读取时 SQLite `DatabaseBusy` 或 `DatabaseLocked` | 500 | internal | true |
| 其他 rusqlite 读取失败 | 500 | internal | false |

已完成参数解析并进入 Store 读取后，除确认的缺行和幂等冲突外，该次读取的其他错误都是 500。这包括 rusqlite 错误，也包括 Repository 用普通 `ensure!` 报出的关系身份损坏。缺少设置、找不到配置路由和业务参数校验仍是 400，没有把所有 anyhow 改成 500。

health 成功时仍是 `ok:true` 加真实 `schema_version`。`schema_version()` 返回错误时走 `read_error`。测试可在另一条连接执行 `PRAGMA user_version=-1`，让无符号转换失败；随后真实 `GET /api/health` 返回 500/internal，且没有 `ok` 或 `schema_version` 成功字段。

## 3. 安全拒绝

Host、Origin、`sec-fetch-site` 和 `x-peachsh-token` 的判断没有放宽，也没有新增 CORS。拒绝发生在进入 Engine 之前，响应是统一 JSON，并带：

```text
Cache-Control: no-store
X-Content-Type-Options: nosniff
Content-Security-Policy: default-src 'self'; ...
```

正常响应继续由同一个 guard 附加这些头。

## 4. SSE 游标

Run SSE 与 Session SSE 使用同一解析器。

1. 参数名先按字节做一次严格 URL 解码，再识别 `after`。`%61fter` 与明文 `after` 是同一个参数；重复计数包含编码写法。一次解码后仍含字面 `%` 的名字也会拒绝，例如 `other%25=1`；这比“未知参数一律忽略”更严格，但不影响当前定义的 `after` 客户端。
2. `after` 和 `Last-Event-ID` 只接受 `0..=i64::MAX` 的 ASCII 十进制数字。`0` 合法。
3. 空值、负数、正号、空白、非数字和溢出返回 400。非法值不会改成 0，也不会改用另一个来源。
4. 重复 `after` 或重复 `Last-Event-ID` 一律拒绝，值相同也拒绝。逗号拼接不是合法数字。
5. 两者都合法时 header 优先。都缺省时使用 0。
6. 参数或对象错误在开流前返回 JSON。不会先回 200，再把输入错误放进流。

`seq` 是数据库全局序号。同一个 Session 内允许间隔，只要求后续事件严格大于游标。大于当前最大序号的合法游标保持等待，不倒退、不重放旧事件。

## 5. SSE 帧与失败

普通事件：

```text
id: <seq 的十进制字符串>
event: <持久化 kind>
data: <现有 Event JSON，含 seq、cursor、session_id、turn_id、task_id、kind、data、at>
```

Session SSE 先按 Session 读取 `legacy_run_id`，再调用既有 `Store::events`。server 不另写关联查询。Run SSE 继续走同一 Store 入口及其旧历史兼容逻辑。

路径参数提取失败也走同一 JSON 错误体。`GET /api/sessions/%FF` 这类非法 UTF-8 路径在进入业务前返回 400/request_failed，不再是 Axum 默认纯文本。未知路由和静态资源不在本轮重做。

SSE 在调用 Axum builder 前用现有 `validate_event_kind` 检查 kind，并拒绝 id/kind 中的 CR、LF 和 id 中的 NUL。损坏字段按存储损坏发一帧 error，不调用会 panic 的 builder，也不改原事件。

服务端游标只在构帧成功并 yield 之后推进。读取、序列化或构帧失败时发一帧：

```text
event: error
data: {"code":"internal","message":"...","retryable":false,"after":<最近成功发出的游标>}
```

没有成功发出事件时，`after` 是请求的有效游标。错误帧没有 SSE `id`，不分配业务 seq，不写 events 表。发出这一帧后结束流，不在内部循环里重复报错。

损坏数据 `retryable=false`。只有错误链里的 SQLite busy/locked 才是 `retryable=true`。现有 `Event` 由 serde 结构序列化，正常持久化事件没有构造出真实编码失败；测试用损坏事件 JSON 证明开流后的读取失败。

客户端断开只结束该订阅。server 不因此取消任务、不新增事件、不调用模型。keep-alive 仍是 10 秒空注释；空闲不是错误。
