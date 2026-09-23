> **现行状态补充（2026-09-23，项目经理）：第一轮已按范围验收结束。** 下文是历史任务原文，其中“可分发执行”“直接实施”“批准并行”等只代表当时安排，不授予本次新任务权限。
> 新对话使用 [员工启动提示词](../../../../员工启动提示词.md)，先读 [AGENTS](../../../../AGENTS.md)、[本人身份](../../个人身份认知.md) 和 [最终审查](<../../审查记录/第一轮/员工C 最终审查.md>)；仅查证历史时无需开工。有明确新任务时按当前派发范围执行。

# 员工 C 第一轮提示词：HTTP 与 SSE 传输契约

状态：负责人批准与员工 B 并行开发。日期：2026-09-23。  
从下一节开始可以完整交给 Grok 4.7。任务是实现和测试，不是只写设计。

## 你的角色与本轮目标

你是 Grok 4.7，员工 C，负责 🍑sh harness 的 HTTP/SSE 适配层。工作目录为 `./`。负责人确定方向和契约，你完成代码、调试、测试与报告。

**本轮让现有 HTTP 查询、请求拒绝和 SSE 订阅拥有明确一致的传输契约：输入无歧义、错误可分类、断线可续传、会话不串流、存储故障不伪装成功。**

你与 B 并行，但不依赖 B 的新连续回合命令完成。B 负责 Engine/Domain/Store/Repository；你负责现有 server 适配和独立 HTTP 测试。

当前工作区已经出现 `SendChatTurn`、`ChatTurnReceipt`、`ChatTurnError` 等 B 的实现，但尚未收到 B 的最终报告并验收。这些代码不能当作已冻结契约。本轮**不挂载 `POST /api/sessions/{id}/turns`**，不预先引用 B 未验收类型、不用假成功或 501 占位路由。后续由负责人安排该写入口联调。

## 一、开工必读与基线

依次读取：

1. `./项目开发守则.md`
2. `./PROJECT-BOOK.md` 顶部当前执行说明及产品边界。
3. `./niuma\员工A\审查记录\第一轮\员工A 最终审查.md`
4. `./niuma\项目经理\开发记录\后续开发注意事项.md`
5. `./niuma\员工B\任务\第一轮\员工B提示词.md`，理解文件所有权和延期接入的连续回合契约。
6. `NoManCode/rust-app/src/server.rs`、`src/domain.rs`、`src/store.rs`、`crates/protocol/src/lib.rs`、`tests/runtime.rs`。

先记录 HEAD、`git status --short` 和你准备修改文件的初始差异。当前包含 A 已验收但未提交的修复，也可能包含 B 正在写入的代码。不得 reset、clean、覆盖、重新初始化 Git 或提交全部别人的改动。

已批准范围内直接实施，不反复等待普通实现细节确认；需要改变领域/schema/权限时，记录最小依赖与建议，继续不依赖该变化的工作。

## 二、并行文件所有权

| 所有者 | 允许写入的文件 |
| --- | --- |
| B | `src/engine.rs`、`src/domain.rs`、`src/store.rs`、`src/repository.rs`、`tests/session_turns.rs`，以及其任务单允许的 `tests/runtime.rs`、`tests/final_acceptance.rs` |
| C | `src/server.rs`、新建 `tests/http_contract.rs`、自己的契约文档和报告 |
| C 可选 | 如有必要，在 `src/server/` 下新增本任务的适配辅助模块，并由 `server.rs` 声明；不改 `src/lib.rs` |

上述代码路径均相对于 `./NoManCode\rust-app`。

你可以读取 B 的代码，不能替 B 修改。不要复制出第二个 Engine、业务状态机、幂等表或数据库连接入口。不要改 Cargo 依赖、protocol、Provider、secrets、workspace、WASM、UI、CLI 或 schema。

不要编辑 B 拥有的测试文件；在 `tests/http_contract.rs` 使用稳定公开接口建立自己的临时夹具。测试中可以连接临时 SQLite 做故障注入，生产 server 不允许直接 SQL。

并行期间只格式化自己拥有的文件。`cargo fmt --all -- --check` 是检查，可以运行；不要用全仓自动格式化改写 B 的进行中代码。全量编译受其他员工中间状态影响时记录具体错误和时间，不“顺手修复”对方文件，也不把未跑通写成通过。

## 三、负责人确定的传输契约

### C-01：保留已验收的成功响应

保留这些已有读取接口的成功 JSON 形状及正常状态码：

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

不擅自改分页、列表排序、ID、Session 与 legacy Run 的映射或事件 envelope。原有 `POST /api/runs`、Task resume/cancel 等业务处理继续委托既有 Engine；你只收口其传输错误，不重新实现业务规则。

health 在 schema 读取失败时返回明确错误，不得继续使用 `ok:true` 配合 `schema_version:0` 伪装成功。

### C-02：错误响应统一，分类不靠文案

沿用 protocol 的 `ErrorBody`，并保留现有兼容字段 `error`：

```json
{"code":"request_failed","message":"可理解的安全错误说明","retryable":false,"error":"同一安全说明"}
```

本轮明确的 HTTP 分类：

| 情形 | HTTP / code | retryable |
| --- | --- | --- |
| 无效参数、无效 JSON、字段类型错误、游标或 key 非法 | 400 / request_failed | false |
| JSON 接口缺少或不支持 Content-Type | 415 / request_failed | false |
| 超出既有 1 MiB 请求体限制 | 413 / request_failed | false |
| 错 Host/Origin、cross-site、缺少或错误的写请求 token | 403 / forbidden | false |
| 所查对象确实不存在 | 404 / not_found | false |
| 既有幂等冲突 | 409 / conflict | false |
| 读取接口的数据损坏或数据库故障 | 500 / internal | 明确瞬时 busy/locked 可 true；其他 false |

对既有 JSON 输入接口处理 Axum 提取失败，避免绕过错误契约返回默认纯文本。未知路由和静态资源不要求在本轮统一重做。

区分查无记录与查询失败：使用可 downcast 的错误及错误链，如 rusqlite 的 `QueryReturnedNoRows`；不能把 `.map_err` 的所有结果都映射 404，也不能匹配中文错误字符串。保留既有幂等冲突的结构化分类。

不要全局把所有 anyhow 错误改成 500，以免把现有业务参数校验错误误分类。本轮重点收口输入、读取和传输边界；尚未有类型的领域错误留给 B/负责人统一处理，并在报告指出。

错误信息使用既有安全处理，不返回原始请求体、凭据、原始 SQL 或不必要的内部路径。不要修改 secrets 算法。

### C-03：安全判断保持，拒绝响应也一致

保留现有 Host、Origin、`sec-fetch-site` 和 `x-peachsh-token` 检查，不放宽跨域/认证，不添加通配 CORS。

guard 的提前拒绝也返回统一 JSON，并带上现有 `Cache-Control: no-store`、`X-Content-Type-Options: nosniff` 与 CSP 等相同安全头；正常响应继续保持这些头。不因错误格式化而让被拒绝请求进入 Engine。

### C-04：明确 SSE 游标输入

Run 和 Session 两种 SSE 使用同一个游标解析规则：

1. `after` query 与 `Last-Event-ID` header 都只接受 `0..=i64::MAX` 的 ASCII 十进制数字；0 合法。
2. 空值、负数、正号、空白、非数字、溢出均返回 400；不要静默改为 0 或退回另一个来源。
3. 重复 `after` 参数、重复 `Last-Event-ID` header 均拒绝，包括值相同时；逗号拼接值不是合法数字。
4. 两者都提供时必须都合法，然后 header 优先。均未提供使用 0。
5. `seq` 是全局序号，同一个 Session 内可以有间隔；只要求严格递增，不能要求连续加一。
6. 比当前最大 seq 更大的合法游标保持等待，不倒退、不重放旧事件；只有未来出现大于该游标的事件才能返回。

开流前的参数/对象错误按 HTTP JSON 返回；不能先发送 200 再把输入错误藏在流中。

### C-05：SSE 续传与作用域

- 普通事件的 SSE `id` 等于数据库 `seq` 的十进制字符串，`event` 等于持久化 `kind`，`data` 保留现有 Event JSON 和兼容字段。
- Session SSE 只读该 Session 的事件；Run SSE 继续使用已有映射和旧历史兼容逻辑。不能在 server 重建另一套关联查询。
- 重连只返回 `seq > 游标` 的事件；一个 Session 多个 Turn 的事件可以正常续传，两个 Session 的交错事件不得串流。
- 客户端主动断开时释放该订阅，不取消后台业务任务、不新增任务事件，也不触发模型调用。
- 保留现有 keep-alive；空闲不是错误，事件稍后到达时流仍能收到。

### C-06：开流后的失败不丢游标

在事件成功序列化并准备发出后才推进服务端的已发游标；不能先把失败事件的 seq 记作已发。这里的游标只代表服务端已发出，不等于客户端已经确认接收。

流建立后读取/编码发生错误：发出一次安全 `event: error` 帧，然后结束流。错误 data 含 `code/message/retryable` 和 `after`，其中 after 为最近成功发出的游标；没有发出事件时为请求的有效游标。

错误帧不分配新业务 seq、不推进游标、不写入 events 表；不得因为只退出内部 for 循环而持续重复报错。不要把“连接先成功”算成整个流成功，也不要默默关流。

数据损坏 retryable=false；明确瞬时存储 busy/locked 可 true；不能将所有未知故障一律称为可重试。现有 Event 类型若无法构造真实编码失败，可以说明这一不可达边界并核对代码顺序，不要修改领域类型制造虚假测试。

## 四、本轮验收矩阵

主要测试写到 `NoManCode/rust-app/tests/http_contract.rs`。使用临时数据库、独立夹具和 `127.0.0.1:0` 的本机 mock，禁止连接用户运行实例或真实 Provider。

| 编号 | 场景 | 必须证明 |
| --- | --- | --- |
| H1 | 正常 health/Project/Session/Turn/Run 查询 | 已有成功响应形状、映射和安全头保持 |
| H2 | 查无对象与临时数据库/JSON 损坏 | 404 与 500 正确区分，不伪报不存在；health 失败路径不回 ok:true/schema0 |
| H3 | Host/Origin/cross-site/写 token 拒绝 | 403 结构化 JSON 和安全头；数据库与 Provider 调用数不变 |
| H4 | JSON 语法错误、类型错误、Content-Type、超大 body | 400/415/413 按契约，统一响应；请求未进入业务执行 |
| H5 | 游标完整输入组合 | 缺省、0、header 优先；非法、负数、空白、溢出、重复均按 C-04 拒绝 |
| H6 | SSE 正常事件 | id/seq/cursor/kind、Session/Turn 归属和兼容字段正确 |
| H7 | 消费若干事件后主动断开并重连 | 后续事件严格大于最后消费 seq，不重放、不遗漏；断开不取消任务 |
| H8 | 两个 Session 的交错事件及同 Session 多 Turn | 范围隔离正确，允许全局 seq 间隔，不能用显示名归属 |
| H9 | 空闲后追加事件、未来游标 | 后到事件可收到；未来游标不重放已有历史且不被重置 |
| H10 | 已成功收到事件后注入临时数据库错误 | 一次 error 后结束，after 不跳过未发事件，错误不新增领域记录 |
| H11 | 既有 `/api/runs` 幂等与坏 key | 原回放/409 保留；非法 key 拒绝且不多调用 Provider |

多 Turn 夹具可使用 A 已验收的 `Store::commit_turn` 构造，不依赖 B 的 `send_chat_turn`。故障注入只动测试临时库；如用损坏事件 JSON，先实际读到一个正常事件，再破坏后续事件，确保测试确实证明“开流后失败”。

SSE 是长流，不要用无限收集整个 response body 的方式测试。使用可控信号、事件计数、明确 timeout，读取所需事件后关闭测试流；避免任意长 sleep。每个坏输入测试核验错误类别、目标前置条件和业务未执行结果，不只断言失败。

## 五、交付与联合验收

完成代码后，在 `./NoManCode\rust-app` 使用项目脚本：

```powershell
pwsh -NoProfile -File build.ps1 -Action test
pwsh -NoProfile -File build.ps1 -Action fmt
pwsh -NoProfile -File build.ps1 -Action clippy
pwsh -NoProfile -File build.ps1 -Action wasm-check
git diff --check
```

记录自己的新增测试与全量结果。全量结果若受 B 进行中代码影响，分开记录已验证部分和待联合复验项，不能假报全绿。不要启用默认忽略的付费 live 测试，不提交或发布全工作区代码。

写出两个文件：

1. `./niuma\员工C\任务\第一轮\员工C HTTP-SSE契约.md`：现有接口、错误映射、游标、SSE 重连/故障示例；描述实际实现，不能仅抄任务要求。
2. `./niuma\员工C\提交报告\第一轮\员工C（HTTP与SSE传输契约 第1轮报告）.md`：基线、修改文件、C-01～C-06 与 H1～H11 的证据、命令/退出码、未验证项、兼容影响和风险。

新连续 Turn 写入口仅作为后续对接说明：计划由 path 提供 SessionId、body 提供 agent_id/expected_last_turn_id/message、header 提供幂等 key，并返回准确 Turn/Task/replayed。状态码、最终 DTO 与 B 的应用错误映射在联合任务中确定，本轮不声称它已经可用。

负责人收到报告后写入 `./niuma\员工C\审查记录\第一轮\员工C review.md`。C 的传输层可以独立审查；B 的领域契约通过后，再安排新 Turn HTTP 写接口及 B+C 联合验收。你不自行宣布审查通过。
