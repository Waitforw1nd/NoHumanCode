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
| `toolCall`、`toolUpdate`、`toolResult` | 意图整批持久化，逐项发送 `ToolCall`、审查、执行；保存结果后才发送 `ToolResult`。拒绝、审查/执行错误或 `is_error` 会结束派发；剩余意图各保存一个错误结果。没有续轮预算也不会执行副作用。 |
| `error`、`done` | 失败按序发送固定代码和 `Done(Failed)`，成功为 `Completed`，显式中止为 `Aborted`。独立消费每轮的事件通道；丢弃观察者后仍会完成执行与存储。 |
| `abortRun` | `abort(session_id, run_id).await` 等到匹配回合的宿主回调、存储与会话占用结束。重复或过期的中止不影响下一轮。 |
| `compactSession` | `compact(session_id, model, instructions).await -> Result<String>`。对完整选中投影做一次无工具摘要，在同一会话独占下追加 `Entry::Compaction`。返回的 leaf 即 compaction 的 `entryId`。宿主应将此全量摘要的 `firstKeptEntryId` 指向摘要自身，并在后续 `load` 投影中引用它，保留原始分支。 |

`coda-protocol::embedded` 的 Rust 数据结构需要适配后再用 `@coda/contracts` 验证。宿主自行映射压缩忙闲状态，提供 C3 `tokensBefore` 等 codec 元数据，管理身份、持久 run ID 和工作区文件租约。前置 B 支持显式摘要；pi 的自动压缩触发时机、重试事件、部分截断回复的持久化和两种内核的逐事件等价仍待产品适配。旧 pi 存档的一键转换、真实网关和 Bun 工具接线属于 Coda 侧 M3。
