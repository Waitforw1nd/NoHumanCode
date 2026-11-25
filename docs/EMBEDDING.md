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
| `ToolHost::definitions(session_id, run_id) -> Result<Vec<ToolDefinition>>` | 返回本轮已过滤的工具 `name`、`description`、`parameters`。 |
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

### 中止消息的存档与回放

`SessionStore::append` 将新的 `Message::Aborted` 写成一条 C3 v2 的 `message`。`role` 为 `assistant`，`content` 是 `partial` 中按序保存的正文和思考块，`metadata.kernel` 保存 `stopReason` 和 `runId`。以下是一条完整的示例行，`parentId` 指向已有的用户消息或工具结果。

```json
{"id":"stop-1","parentId":"user-1","timestamp":"2026-11-25T12:00:00Z","type":"message","role":"assistant","content":[{"type":"text","text":"已检查第一项"}],"metadata":{"kernel":{"stopReason":"aborted","runId":"run-1"}}}
```

实现了 `Message` 穷举匹配的宿主需要新增 `Aborted` 分支。`load` 按该元数据还原 `Message::Aborted`，`ModelRequest.messages` 使用普通 `Assistant` 文本表示取消。部分回复可能为空。如果完整助手回复或工具意图已写入，独立中止行的 `content` 为空，保留先前回复和逐项工具结果。内核为下一次模型请求（包括压缩）生成一条明确说明用户已取消旧任务的助手文本；若前一条已经是助手消息，就把说明加在末尾。半截内容只进存档，模型看到的是取消说明和新问题。旧 pi 转换继续保留 `metadata.pi.message.stopReason` 及其原始审计字段；适配器回放该字段为 `aborted` 的助手条目时可用原运行 ID，缺失时以原条目 ID 关联。历史权威数据仍由 C3 codec 完整保存。

`coda-protocol::embedded` 的 Rust 数据结构需要适配后再用 `@coda/contracts` 验证。宿主自行映射压缩忙闲状态，提供 C3 `tokensBefore` 等 codec 元数据，管理身份、持久 run ID 和工作区文件租约。前置 B 支持显式摘要；pi 的自动压缩触发时机、重试事件、部分截断回复的持久化和两种内核的逐事件等价仍待产品适配。旧 pi 存档的一键转换、真实网关和 Bun 工具接线属于 Coda 侧 M3。
