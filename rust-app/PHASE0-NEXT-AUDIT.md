# 阶段 0 下一步审计：协议、错误、事件与迁移

日期：2026-09-22  
范围：`rust-app` 当前实现；本文件是实施记录，不替代 [总体架构基线](MASTER-ARCHITECTURE-BASELINE-2026-09-22.md)。

## 已经落地

- `crates/protocol` 已作为独立 workspace crate，`SessionKind` 由请求字段决定。
- protocol 已提供 `ErrorCode`/`ErrorBody`，HTTP 仍保留旧的 `error` 文本字段，便于迁移期前端兼容。
- SQLite 已升到 schema 5：`runs.kind`、`schema_migrations` 和旧 `chat-session` 数据的一次性兼容回填。
- SSE 事件读取失败会发出 `error` 事件后结束流；客户端可以用最后游标重新连接。
- 构建脚本现在以 workspace 为检查边界，protocol 的单元测试不会被漏掉。

## 当前事实与风险

### 错误响应

`server.rs` 的 `ApiError` 当前仍把大多数 `anyhow::Error` 映射成 HTTP 400、`request_failed`。这保证了旧客户端能显示文本，但新客户端还不能可靠地区分参数错误、资源不存在、冲突、权限拒绝、限流和上游故障。

最小下一步：为明确的路由入口增加 `ApiError::with_code(status, code, retryable, error)`，先覆盖资源不存在、幂等冲突和上游限流；保留 `From<anyhow::Error>` 作为未分类兜底，避免依靠中文错误文本分类。未知错误不要自动重试。

### SSE 事件

SQLite 中事件名暂时仍是字符串，因为插件和旧记录可能带有扩展事件。核心事件已经有固定集合（status、delta、tool_start、tool_result、file_backup、file_restore、error 等），但数据库读取和 JSON 序列化仍需要处理错误，不能使用 `unwrap()`。

事件名仍保留字符串以兼容历史表；数据库读取和 JSON 序列化现在会转成可见的 `error` 事件，并带 `after` 游标。后续再在 protocol 中增加可扩展的 `EventKind`/typed envelope，不立刻把历史表的 `kind` 改成 SQL 枚举。

### Schema 迁移

当前迁移可从 v1 升到 v5，且重复打开数据库可安全执行。现有测试验证版本、幂等表和迁移标记，但还应验证：

1. v4 中包含 `chat-session` 旧成员名的 run 会回填为 `chat`；普通 team 不会被误判；
2. 迁移中断后再次打开可继续完成；
3. v5 数据再次打开不会重复改写任务或追加迁移记录。

后续新增表或列应使用单次迁移标记，并在同一数据库事务内完成“变更 + user_version + 标记”；迁移失败时不要将版本号提前推进。

### 持久化错误

后台任务终态现在会检查 `save_task` 和 `event` 的结果，失败时写入主机日志并尽力追加 `persistence_error` 事件。数据库彻底不可写时仍无法凭空制造持久记录，因此后续要在 UI 增加“结果未可靠保存”状态，并把持久调度器作为阶段 2 的硬门槛。

### Leptos CSR 空壳

已安装 `wasm32-unknown-unknown`，并固定 Leptos 0.8.15。`crates/ui` 现在同时通过主机检查和 wasm32 检查，包含一个只渲染协议状态的 Rust/WASM 对话壳；它没有任何 Provider Key、文件、进程或 SQLite 权限。完整页面尚未接入 Axum 静态资源，Host 继续保持现有 HTML/JS fallback。后续垂直切片应先接入 Project/Session/Turn 读取模型，再迁移对话和事件流，避免出现只能显示空壳的“假迁移”。

## 验收门禁

在阶段 0 进入对话 UI 前，至少运行：

```text
build.ps1 -Action fmt
build.ps1 -Action check
build.ps1 -Action test
build.ps1 -Action clippy
build.ps1 -Action wasm-check
```

以上脚本现在覆盖整个 Cargo workspace；真实模型测试仍需单独设置临时 Key，不能把 Key 写入配置或日志。
