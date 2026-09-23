# Protocol migration test plan

当前代码还没有未来协议所需的 `RunKind` enum，也没有 `RunRequest.kind` 字段；数据库当前最高 schema 为 4（`SCHEMA_VERSION == 4`）。因此本轮不添加会导致编译失败的 `tests/protocol_migration.rs`，只记录待协议实现后启用的测试要求。

## 目标测试文件

实现 `RunKind`、`RunRequest.kind` 与 schema 5 后，新增 `tests/protocol_migration.rs`，覆盖以下行为：

1. **RunKind serde**
   - `serde_json::from_str::<RunKind>("\"chat\"")` 得到 `RunKind::Chat`。
   - `serde_json::from_str::<RunKind>("\"team\"")` 得到 `RunKind::Team`。
   - `RunKind::Chat` 和 `RunKind::Team` 序列化为稳定的协议字符串（预期分别为 `"chat"`、`"team"`），以保证 HTTP/持久化边界不暴露 Rust 变体名。
   - 未知值（例如 `"batch"`）反序列化失败。

2. **RunRequest.kind 反序列化**
   - 含 `kind: "chat"` 的最小合法请求可反序列化，并保留 `RunKind::Chat`。
   - 含 `kind: "team"` 的最小合法请求可反序列化，并保留 `RunKind::Team`。
   - 缺少 `kind` 的请求按协议约定验证默认值（若迁移要求兼容旧客户端，应断言默认值；若要求显式字段，应断言反序列化失败）。该选择需在业务实现时固定，不能由测试猜测。
   - 未知 `kind` 必须拒绝，避免静默降级到错误的运行模式。

3. **未来 schema 拒绝**
   - 创建带 `PRAGMA user_version = 5` 的数据库后调用 `Store::open`，必须返回错误。
   - 错误应明确指出数据库 schema 高于当前支持版本，并且打开操作不得把数据库降级或覆盖。
   - 保留现有 schema 4 迁移测试，确认 schema 4 仍可升级到当前版本；schema 5 拒绝测试只验证“未来版本拒绝”，不修改业务迁移逻辑。

## 建议测试骨架

```rust
use peachsh::domain::{RunKind, RunRequest};
use peachsh::store::Store;
use serde_json::json;

#[test]
fn run_kind_uses_stable_wire_values() { /* serde assertions above */ }

#[test]
fn run_request_deserializes_kind() { /* chat/team + missing/unknown cases */ }

#[test]
fn future_schema_is_rejected() { /* temp DB, PRAGMA user_version=5, Store::open */ }
```

启用前应先确认 `RunKind` 的实际模块路径、wire casing、缺省 `kind` 策略，以及 schema 5 的迁移边界；随后把本计划中的占位断言替换为可编译测试，并运行 `cargo test --test protocol_migration`。
