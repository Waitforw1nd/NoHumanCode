# A-AP12-AUDIT 最小反例

受审提交固定为 `50de5456b8c0d412aff4e90821bdde221f066a2d`。

复验时用 `git archive` 将该提交的 `NoManCode/rust-app` 解到仓库外临时目录，把 `audit_redaction_paths.rs` 放入其 `tests/`，加载 Visual Studio x64 开发环境并设置独立 `CARGO_TARGET_DIR`，再执行：

```text
cargo test --test audit_redaction_paths -- --nocapture
```

探针只调用候选公开的 `secrets::redact_persisted`，模拟 `finish_approval` 中消息内容与事件结果的两条真实脱敏次序。输入 `bearer abc123` 是模拟数据，不是凭据。完整源码和 target 仅为临时构建材料，运行后已移出仓库，不属于交付。
