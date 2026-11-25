# Coda Bot 进程内接入

Tauri 主进程依赖同一提交的 `coda` 与 `coda-protocol`。为 `coda` 指定 `default-features = false`，即可只编入内核。`coda::embedded` 重导出协议类型、`CancellationToken` 与 `BoxFuture`。独立 CLI 使用默认的 `standalone` 特性。

```toml
# 将 REV 替换为接入环境可获取的同一个完整提交 SHA。
coda = { git = "https://github.com/Waitforw1nd/NoHumanCode", rev = "REV", default-features = false }
coda-protocol = { git = "https://github.com/Waitforw1nd/NoHumanCode", rev = "REV" }
```

```rust,ignore
use coda::EmbeddedEngine;
let engine = EmbeddedEngine::new(gateway, reviewed_tools, sessions);
let mut events = engine.run(options)?; // 调用方位于 Tokio runtime 中
while let Some(event) = events.recv().await {
    relay(event);
}
```

## 三个宿主接口

下表以 `async` 简写异步签名，实际 trait 方法返回 `BoxFuture<'a, anyhow::Result<T>>`，可用 `Box::pin(async move { ... })` 实现。三个实现均为 `Arc<dyn Trait>`，须满足 `Send + Sync`。完整可运行示例见[宿主集成测试](../tests/embedded_kernel.rs)。

| 接口 | 宿主实现 |
| --- | --- |
| `ModelTransport::preflight(&ModelRequest) -> Result<()>` | 使用本轮 `session_id`、`run_id`、`model.id`、`model.provider` 和 `settings` 做本地权限、路由、能力及 token 预算检查。`provider` 承接 C3 的 `customProviderId`。计入系统指令、工具和完整输入，确认传输能执行相同的请求。 |
| `async complete(&ModelRequest, emit, CancellationToken) -> ModelReply` | 在线经 SSO，离线交给选中的提供商。以 `emit(ModelDelta)` 报告有 `block_id` 的文本或思考；每次检查回调结果。正常返回完整块及可选用量；连接异常、截断、解析失败作为错误返回。 |
| `ToolHost::definitions(session_id, run_id) -> Result<Vec<ToolDefinition>>` | 每次生成请求预检前读取一次当前工具集。宿主处理 `setActiveTools()` 时更新自己的工具注册表，下次读取即可反映新增、移除或参数变动。内核检查调用 ID，并按该次请求的工具名称集合校验回复。宿主校验参数及当前权限。已提交的工具批次保留原调用 ID 和结果。 |
| `async review(&Invocation, CancellationToken) -> bool`、`async execute(&Invocation, update, CancellationToken) -> ToolResult` | 以 `(session_id, run_id, call.id)` 关联 `reviewToolCall()`、执行和进度。宿主缺少授权时拒绝；执行只接收已审查的调用。审查、执行与模型传输须在取消后结束子工作并返回。 |
| `async SessionStore::load(session_id) -> Snapshot` | 在宿主的会话锁、工作区租约和 codec 下选择 leaf，返回对应分支的消息投影与修订号。恢复时保留未完成意图，内核会拒绝重派。 |
| `async append(session_id, expected_revision, &[Entry]) -> String` | 在同一次原子提交中校验旧修订、追加整批条目并返回新 leaf。快照修订须能检测跨写入者冲突。保留原始条目、其他分支与宿主 metadata。失败时宿主不得推进内存 leaf。 |

## C3 v2 对照

| C3 命令或事件 | 适配 |
| --- | --- |
| `createSession`、`resumeSession` | 宿主分配会话 ID、选存储、验证 JSONL 和 leaf，之后将 `session_id` 传给内核。同一工作区复用一个 `EmbeddedEngine` 或它的 `clone()`。 |
| `run` | `run(RunOptions) -> Result<UnboundedReceiver<Event>>`。模型、system prompt、文字/图片、思考与输出限制随本轮传入。活动会话拒绝第二个回合；提交过的 run ID 在该实例生命周期内不可复用。 |
| `start`、`text`、`thinking`、`usage` | 分别映射 `Event::Start`、`Text`、`Thinking`、`Usage`。将 Rust 字段映射到 C3 的 `modelId`、`customProviderId`、`blockId`、`cacheRead`、`cacheWrite`。历史思考只用于存档，模型输入投影移除它。 |
| `toolCall`、`toolUpdate`、`toolResult` | 内核先保存整批工具意图，再逐项发送 `ToolCall`、审查、执行，保存结果后发送 `ToolResult`。工具自身失败（如命令非零退出）时，宿主应返回 `Ok(ToolResult { is_error: true, .. })`，在 `content` 中提供错误信息。同批后续调用照常审查和执行，下一次模型请求按顺序收到所有工具结果。 |
| `error`、`done` | 失败按序发送固定代码和 `Done(Failed)`，成功为 `Completed`，显式中止为 `Aborted`。独立消费每轮的事件通道；丢弃观察者后仍会完成执行与存储。 |
| `abortRun` | `abort(session_id, run_id).await` 等待匹配回合的宿主清理、存储追加与会话占用结束。用户消息已提交时，成功的中止先保存 `Entry::Message(Message::Aborted { run_id, partial })`，再发送 `Done(Aborted)`。工具批次会先保存每个意图的结果。中止行写入冲突发出 `SESSION_APPEND` 和 `Done(Failed)`。开始首次用户追加前观察到中止时保持原 leaf。如果追加正在进行，先等待其结果；成功后再追加中止行。 |
| `compactSession` | `compact(session_id, model, instructions).await -> Result<String>`。对完整选中投影做一次无工具摘要，在同一会话独占下追加 `Entry::Compaction`。返回的 leaf 即 compaction 的 `entryId`。宿主应将此全量摘要的 `firstKeptEntryId` 指向摘要自身，并在后续 `load` 投影中引用它，保留原始分支。 |

审查拒绝（`Ok(false)`）、审查出错（`Err`）和宿主执行接口出错（`Err`）分别以 `TOOL_DENIED`、`TOOL_REVIEW` 和 `TOOL_EXECUTION` 结束回合。同批剩余调用各保存一个 `TOOL_NOT_EXECUTED` 结果。取消或事件预算耗尽会停止后续派发。没有下一次模型请求的预算时，内核记录工具意图和跳过结果，以 `REQUEST_LIMIT` 收尾。

### 回合中插话与取回

需要接收插话时，使用 `engine.run_with_input(options)?`。返回值 `RunningTurn` 包含原有的事件接收端 `events` 和可克隆的 `TurnInput`。宿主按 `(session_id, run_id)` 保存句柄，并在回合收尾时取回余项。只消费事件的调用方仍可使用 `run(options)`。

| 宿主操作 | Rust 接口和所有权 |
| --- | --- |
| `steer(text)` | 构造 `SteeringMessage::User { content }`，调用 `input.enqueue(message)`。`Ok(())` 表示进入本回合队列。`EnqueueError::Closed`（回合已关闭）和 `Full`（超过容量）均保留原值，可用 `into_message()` 取回。每个回合最多容纳 1024 项、合计 64 MiB 序列化 JSON，包含选中且仍在写入的批次。 |
| `sendCustomMessage(..., { deliverAs: "steer" })` | 使用 `SteeringMessage::Custom(CustomMessage { custom_type, content, display, details })`，和用户消息共用一个先进先出队列。 |
| `getSteeringMessages()`、`hasQueuedCustomMessage(type)` | `input.pending()` 返回可取回消息的快照。宿主可按变体提取用户文本或匹配 `custom_type`。 |
| `clearQueue()` | `input.clear_queue()` 立即移出并返回当前待投递项。已经选入当前请求的消息由该次写入决定归属。 |
| 完成、失败、取消后收尾 | `input.take_remaining().await` 等待本回合释放会话占用，按顺序移出余项。所有克隆共享这次取回，后续再取为空。宿主即使关闭了事件订阅，也可用保留的句柄取回。 |

```rust,ignore
let mut turn = engine.run_with_input(options)?;
register_input(session_id, run_id, turn.input.clone()); // 供宿主的插话入口调用
while let Some(event) = turn.events.recv().await {
    relay(event);
}
if let Some(batch) = turn.input.take_unconfirmed().await {
    reconcile_with_store(batch); // 仅在宿主写入回执中断时需要
}
return_to_host(turn.input.take_remaining().await);
```

**交付边界。** 每次准备生成请求时，内核选取当时排队的全部消息，用最新工具声明和包含插话的完整请求做预检。预检通过后，以一次修订号校验和原子追加写入整批消息，再按原顺序逐条发出 `Event::InputDelivered { message }`。宿主可据此更新排队标记及显示历史，存档只由 `SessionStore::append` 写一次。交付事件确认消息已进入会话历史。若取消发生在追加期间，内核等待结果；追加成功的消息仍确认交付，模型请求可能因取消而没有发出。存储返回 `Err` 须表示整批未提交；宿主负责在返回前核实存在歧义的写入结果。预检或追加明确失败时，选中的消息重新排在后来入队的消息之前。若写入回调在给出回执前 panic，或运行时丢弃了正在写入的任务，`take_unconfirmed().await` 单独交还该批原始插话与写入时的 `expected_revision`，仅可取一次。宿主应按本回合的写入记录核对该修订上的提交结果，然后处理后续 `take_remaining()` 中明确未提交的余项。待核实批次不会自动重投。进程退出后的队列恢复由宿主持久化策略负责。

**与 pi 的时序。** 对照 Coda 桌面主聊天所用 `@earendil-works/pi-agent-core` 与 `pi-coding-agent` 1.5.0：

| 情形 | pi 与嵌入内核的行为 |
| --- | --- |
| 工具执行时收到插话 | 两者都等待同一回复中的整批串行工具结束。A 执行时收到插话，顺序为 A 结果、B 结果、插话、下一次请求。 |
| 一次投递几条 | pi 默认 `one-at-a-time`，Coda 主聊天设置 `steeringMode = "all"`。嵌入接口固定一次取全部，保留用户消息与自定义消息的相对顺序。选批之后抵达的消息留给下一个请求边界。 |
| 无工具的回复结束 | 两者都检查待处理的插话，有消息即可续轮。嵌入内核受本回合 `max_requests` 约束；最后一次请求已给出完整答复时正常完成，剩余插话退回宿主。空队列的终止判定与入队互斥，关闭后的入队直接返回原消息。 |
| 取消和退回 | pi SDK 的 `abort()` 等待空闲，Coda 在 `finally` 调 `clearQueue()`，其返回值列出 steering / follow-up 文本。嵌入内核的 `take_remaining()` 也保留自定义消息的完整值。本入口只处理 `deliverAs: "steer"`，余项由宿主决定保存或在新回合提交。 |

**自定义消息的 C3 v2 映射。** `Message::Custom` 对应 `type: "customMessage"`，`custom_type` 映射到 `name`，保留 `content` 与 `display`。`details: None` 省略字段，`Some(Value::Null)` 对应 `"details":{"value":null}`，其他 JSON 同样包装为 `{"value": ...}`。`load` 应恢复 `Message::Custom`。生成和显式压缩均只把正文投影为普通 `User` 消息，包括 `display: false` 的消息。穷举匹配的宿主须处理新增的 `Message::Custom` 与 `Event::InputDelivered`。

### 中止消息的存档与回放

`SessionStore::append` 将新的 `Message::Aborted` 写成一条 C3 v2 的 `message`。`role` 为 `assistant`，`content` 是 `partial` 中按序保存的正文和思考块，`metadata.kernel` 保存 `stopReason` 和 `runId`。以下是一条完整的示例行，`parentId` 指向已有的用户消息或工具结果。

```json
{"id":"stop-1","parentId":"user-1","timestamp":"2026-11-25T12:00:00Z","type":"message","role":"assistant","content":[{"type":"text","text":"已检查第一项"}],"metadata":{"kernel":{"stopReason":"aborted","runId":"run-1"}}}
```

实现了 `Message` 穷举匹配的宿主需要新增 `Aborted` 分支。`load` 按该元数据还原 `Message::Aborted`，`ModelRequest.messages` 使用普通 `Assistant` 文本表示取消。部分回复可能为空。如果完整助手回复或工具意图已写入，独立中止行的 `content` 为空，保留先前回复和逐项工具结果。内核为下一次模型请求（包括压缩）生成一条明确说明用户已取消旧任务的助手文本；若前一条已经是助手消息，就把说明加在末尾。半截内容只进存档，模型看到的是取消说明和新问题。旧 pi 转换继续保留 `metadata.pi.message.stopReason` 及其原始审计字段；适配器回放该字段为 `aborted` 的助手条目时可用原运行 ID，缺失时以原条目 ID 关联。历史权威数据仍由 C3 codec 完整保存。

`coda-protocol::embedded` 的 Rust 数据结构需要适配后再用 `@coda/contracts` 验证。宿主自行映射压缩忙闲状态，提供 C3 `tokensBefore` 等 codec 元数据，管理身份、持久 run ID 和工作区文件租约。前置 B 支持显式摘要；pi 的自动压缩触发时机、重试事件、部分截断回复的持久化和两种内核的逐事件等价仍待产品适配。旧 pi 存档的一键转换、真实网关和 Bun 工具接线属于 Coda 侧 M3。
