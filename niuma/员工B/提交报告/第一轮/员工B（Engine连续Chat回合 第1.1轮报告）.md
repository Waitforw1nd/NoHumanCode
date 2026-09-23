> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 第 1.1 轮报告：Engine 连续 Chat 回合返工

日期：2026-09-23  
角色：员工 B。本报告只记录返工实现和实测证据，不宣布审查通过。  
第 1 轮报告保留，不覆盖。

## 1. 基线

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`  
未创建提交，未发布，未替换运行实例。

A 的未提交修复继续保留。C 正在修改 `rust-app/src/server.rs` 和 `rust-app/tests/http_contract.rs`；本轮没有编辑这两个文件，也没有接新的 HTTP 写路由。

本轮写入仍限于：

- `rust-app/src/domain.rs`
- `rust-app/src/engine.rs`
- `rust-app/src/store.rs`
- `rust-app/src/repository.rs`
- `rust-app/tests/session_turns.rs`

Schema 仍是 6。没有新增列或迁移。

## 2. B-R1～B-R5

### B-R1 · 最新回合不再被旧轮次后续事件改写

入口：`Repository::latest_committed_turn`，`rust-app/src/repository.rs`。追加和 resume 都调用它。

修法：按每个 Turn 的 `MIN(e.seq)` 分组，再取最大 opening seq。所有 Turn 都必须有所属事件；任一缺失返回 `ChatTurnError::CorruptState`，不跳过、不按最后写入事件猜测。这个查询不能单独证明“只删除创建事件、后续事件仍在”的损坏，报告不这样声称。

证据：`t6_same_second_uuid_order_does_not_decide_latest`。两个真实 Turn 同秒创建，较新 Turn 被改成字典序更后的 ID。对仍然存在的旧 Turn 追加返回 `StalePredecessor`。给旧 Turn 补 completed 事件后，latest 仍是较新 Turn；旧 Turn resume 被拒绝，状态和 output 不变。

### B-R2 · 前序内容变化不再被旧候选漏掉

入口：`Store::append_chat_turn` 的 `candidate_continues_snapshot`，`rust-app/src/store.rs`。分类在比较之前。

修法：立即写事务重读当前前序。候选必须是该快照再加一条 user message；route、workspace、name、role、route_id、max_rounds 和消息前缀必须一致。不一致返回新增的 `ChatTurnError::PredecessorChanged`，不提交。同 key 已提交结果仍先回放。

证据：`t10_historical_resume_loses_to_a_newer_turn` 后半段。先构造不含中间 resume 的候选，再真实 `Engine::resume` 并等待完成，然后用独立 `Store::open` 提交原候选。结果是 `PredecessorChanged`，Turn 数不增加，候选任务不存在。

### B-R3 · 畸形上下文在提交和派发前拒绝

入口：`chat_message_prefix_ok`，`rust-app/src/engine.rs`。

修法：空历史拒绝。`tool_calls` 缺失合法，非数组非法。工具调用必须有非空 id、`type=function` 和 function 对象。content 必须是非空字符串，或非空 text parts。未配对 tool 消息拒绝。失败是 `UnsupportedSession`，不删除字段、不裁剪。

证据：`t7_busy_non_chat_corrupt_order_and_unpaired_tools_are_refused`。未配对 tool_call 和 `tool_calls` 为对象的前缀都返回 `UnsupportedSession`，对象数和 Provider 调用不增加。合法纯文本前缀仍由 T1 成功覆盖。

### B-R4 · 单 Agent 和 workspace 归属由 Store 裁决

入口：`append_chat_turn` 中的 Agent 计数和 `same_project_path`。

修法：同一写事务确认 Session 恰有一个 Agent，且就是命令指定 Agent。前序 workspace 与 `Project.root_path` 用 `canonicalize` 比较；不匹配拒绝，不改写成项目根。只改全局 Settings.workspace 的成功用例保留。

证据：同 T7 后半段。公开 `insert_agent` 增加第二个 Agent 后，原 Agent 追加返回 `UnsupportedSession`，Turn 数不增加。前序 workspace 改为 `X:/unrelated-project` 后同样拒绝。T11 确认全局 workspace 变化不会切换已有项目。

### B-R5 · 验收夹具已改到目标分支

| 条目 | 现在实际断言 |
| --- | --- |
| T3 | 两个 Store 连接同 key 并发，一个创建、一个回放。Engine 回执返回后，同 key 重试 `replayed=true`，mock 中该消息只出现 1 次。Engine 内存 gate 不能让两个 `send_chat_turn` 同时越过提交，所以没有把死锁式双 spawn 当作并发证据 |
| T4 | 同 key 改消息、Session、Agent、前序，以及 legacy-only key，均为 `IdempotencyConflict`，对象和调用不增加 |
| T5 | 两个独立 Store 连接、不同 key、同一前序，成功数是 1，失败是 `StalePredecessor` |
| T6 | 两个真实同秒 Turn；旧 ID 是 `StalePredecessor`，不是不存在 ID 的 `NotFound`；补旧事件不能重开 |
| T7 | 缺失 Agent/Turn 是 `NotFound`；Team、工具历史、多 Agent、错误归属 workspace、坏 tool_calls 是 `UnsupportedSession`；缺首事件是 `CorruptState` |
| T8 | 事件 trigger 失败后对象、幂等记录、事件和调用都不增加，并且 `!engine.is_busy()` |
| T9 | 三个独立夹具：已取消重开后 `recover` 影响数为 0 且仍 cancelled；真实 mock 进入 running 后关闭并重开，变 interrupted 且保留 delta；只 `append_chat_turn` 不 launch 后重开变 interrupted，同 key 回放不 busy。显式 resume 才再次调用 |
| T10 | 历史 resume 拒绝，加上 B-R2 的 resume 完成后旧候选冲突 |
| T11 | host 变化失败且调用不增加；route 删除失败且调用不增加；全局 workspace 变化不切换项目；累计历史超过 1,500,000 字节、短新消息返回 `InvalidInput`；限内短消息成功 |
| T12 | 同一 Session 多 Turn 事件归属正确，seq 严格向后 |

没有删除原断言来换通过数。T3 的 Engine 双连接同时进入 `send_chat_turn` 不能作为并发证据，因为现有内存 gate 会串行化；跨连接并发由 Store 直接追加证明。

## 3. B-C1 与 B-C2

B-C1 本轮已处理，但仍是诊断改进，不是 P1。确定不存在的 Session、Agent、Turn 返回 `NotFound`。Agent `created_at < 0` 的解码失败返回新增 `CorruptState`，不再丢成 NotFound。缺少首事件也返回 `CorruptState`。没有改 protocol，也没有替 C 映射 HTTP。

B-C2：本轮门禁使用的真实宿主是 PowerShell 7.6.5：

`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`

第 1 轮把 PATH 中的 Windows PowerShell 5.1 shim 说成“本机没有 PowerShell 7”不准确。机器上存在上述 PS7；当时失败的是测试进程 PATH 没有 `pwsh.exe`。本轮测试 PATH 仍加入既有 shim，供旧命令测试启动 `pwsh.exe`，不把该 shim 的 5.1 版本当作 PS7 验证。

## 4. 公共契约变化

`ChatTurnError` 新增：

- `PredecessorChanged`：前序 ID 仍是最新，但内容已变化。
- `CorruptState`：行存在但不能解码，或缺少判序事件。

`NotFound` 现在只表示确定不存在。C 以后接路由时不要把 `CorruptState` 映射成 404。本轮没有定义 HTTP 码。

## 5. 实际检查

执行目录：`./rust-app`。宿主是上面的 PowerShell 7.6.5，`build.ps1` 配置项目既有 MSVC / Windows SDK。

| 检查 | 结果 |
| --- | --- |
| `pwsh -NoProfile -File build.ps1 -Action test` | 94 通过，0 失败，1 忽略；退出码 0 |
| `build.ps1 -Action fmt` | 退出码 0 |
| `build.ps1 -Action clippy` | workspace/all-targets、`-D warnings`；退出码 0 |
| `build.ps1 -Action wasm-check` | 退出码 0 |
| `git diff --check` | 退出码 0 |

94 项包含 C 已存在的 11 个 `http_contract` 测试。它们在本次全量运行中通过，但这不是 C 的验收结论，也不是本轮修改。B 自己的 `session_turns` 为 12 通过。忽略项仍是未执行的付费 `xpeach_native_stream_and_coding_tool`。

`proc-macro-error2` future-incompat 提示仍在，不是本次失败。

## 6. 未完成

1. HTTP/CLI 写路由仍等待 B 验收后由 C 接入。`PredecessorChanged` 和 `CorruptState` 的 HTTP 映射需要负责人确认。
2. `latest_committed_turn` 不能识别“创建事件被删但后续事件还在”。当前只保证完全没有所属事件时拒绝。
3. Engine 内存 gate 仍串行化同一实例的 `send_chat_turn`。跨进程/跨连接的唯一性由 Store 立即事务保证，不把内存锁写成数据库锁。
4. 未执行付费 live 测试。未提交，未覆盖 A 或 C 的文件。
