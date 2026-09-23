> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 C：HTTP 与 SSE 传输契约 第 1 轮报告

日期：2026-09-23。实现和测试已完成。本文不宣布审查通过。

## 1. 任务与代码基线

- HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`
- 继承了员工 A 已验收但未提交的修复，也看到员工 B 正在写入的 `domain.rs`、`engine.rs`、`store.rs`、`repository.rs`、`tests/runtime.rs`、`tests/session_turns.rs`。
- 本轮只写 `rust-app/src/server.rs`、新增 `rust-app/tests/http_contract.rs`、本契约文档和本报告。
- 没有 reset、clean、提交，也没有全仓自动格式化。`main.rs`、protocol、schema、secrets 算法和 B 的文件没有被本轮修改。

## 2. 验收条目

| 条目 | 实现入口 | 证据 | 状态 |
| --- | --- | --- | --- |
| C-01 成功响应保留 | `health`、`projects`、`session`、`session_turns`、`turn`、`runs`、`run` | H1 | 已实现且验证 |
| C-01 health 失败不伪装成功 | `health` 对 `schema_version()` 使用 `read_error`，删除 `unwrap_or_default()` | 代码核对；无独立 HTTP 夹具 | 已实现，HTTP 失败路径未验证 |
| C-02 错误分类 | `ContractJson`、`read_error`、`classify_anyhow` | H2、H4、H11 | 已实现且验证 |
| C-03 安全拒绝 | `guard`、`forbidden` | H3 | 已实现且验证 |
| C-04 游标 | `event_cursor`、`query_cursor`、`cursor_value` | H5 | 已实现且验证 |
| C-05 SSE 续传与范围 | `open_event_stream` 调用既有 `Store::events` | H6、H7、H8、H9 | 已实现且验证 |
| C-06 开流后失败 | 序列化成功后才推进 `sent`；一次 `event: error` 后结束 | H10 | 已实现且验证 |

| 编号 | 证据 | 状态 |
| --- | --- | --- |
| H1 | health/Project/Session/Turn/Run 形状、`legacy_run_id` 映射、安全头保持；Provider 调用 0 | 已验证 |
| H2 | 缺失 Session/Turn/Run 为 404；损坏 `sessions.kind` 为 500/internal/retryable false，不报 not_found，不回显坏值 | 已验证 |
| H3 | 错 Host、错 Origin、cross-site、缺 token、错 token 都是 403 JSON 加安全头；对象数和 Provider 调用不变 | 已验证 |
| H4 | JSON 语法和类型错误 400，缺/错 Content-Type 415，超过 1 MiB 为 413；对象数和 Provider 调用不变 | 已验证 |
| H5 | 缺省和 0 可开流；header 优先；负数、正号、空白、非数字、溢出、重复 query/header 均 400 | 已验证 |
| H6 | SSE `id`、`event`、`seq`、`cursor`、Session/Turn/task 归属和 `at` 与持久化事件一致 | 已验证 |
| H7 | 断开后用最后消费 seq 重连，只收到更大的 seq；不取消任务，不调用 Provider | 已验证 |
| H8 | 两个 Session 交错事件不串流；同 Session 两个 Turn 按 ID 归属；全局 seq 允许间隔 | 已验证 |
| H9 | 当前最大 seq 作为未来游标时不重放历史；追加事件后收到严格更大的 seq | 已验证 |
| H10 | 先读到正常事件，再破坏后续事件 JSON；错误帧 `after` 停在已发游标，不新增 events | 已验证 |
| H11 | 同 key 同请求回放，同 key 改标题 409/conflict；非法 key 400 且对象数不增加，Provider 只被首次成功创建调用一次 | 已验证 |

## 3. 修改文件与公共契约

- `rust-app/src/server.rs`：统一 JSON 错误、读取错误分类、安全拒绝、游标和 SSE 失败结束。
- `rust-app/tests/http_contract.rs`：H1～H11。
- `员工任务/第一轮/员工C HTTP-SSE契约.md`：实际传输契约。
- 成功 JSON 形状、事件 envelope、分页和排序没有改。
- 没有新增 Turn HTTP 写路由，没有改 protocol、schema、权限或凭据算法。

## 4. Schema、迁移、权限、凭据

- Schema：无变化，仍使用现有 schema 6。
- 迁移：无变化。
- 权限：Host、Origin、cross-site 和写 token 检查保持；拒绝响应改为结构化 JSON。
- 凭据：继续使用既有 scrub。错误消息不返回原始请求体或 SQL。没有修改 secrets 算法。

## 5. 失败、取消、恢复、兼容

- 查无记录只认 `QueryReturnedNoRows`，不按中文文案映射 404。
- 读取损坏为 500 且不可重试；SQLite busy/locked 才可重试。
- 非 rusqlite 的业务校验仍是 400，没有全局把 anyhow 改成 500。
- SSE 错误不写 events，不分配业务 seq。客户端断开不取消后台任务。
- 既有 `POST /api/runs`、resume、cancel 仍委托 Engine。本轮只收口其传输错误。
- 领域错误里尚未有稳定类型的部分留给 B 和负责人，没有在 server 猜分类。

## 6. 实际测试

本机没有 `pwsh`。`powershell.exe -File build.ps1` 被执行策略拒绝。下面的全量命令使用：

```text
powershell.exe -NoProfile -ExecutionPolicy Bypass -File ./rust-app\build.ps1 -Action <action>
```

这不是把 Windows PowerShell 改名为 `pwsh`。脚本内容未被修改。

| 命令 | 结果 | 退出码 |
| --- | --- | --- |
| `build.ps1 -Action test` | 库测试 43 通过；adversarial 4 通过；final_acceptance 9 通过；http_contract 11 通过；live 1 忽略。runtime 10 项中 9 通过，`replace_existing_file_preserves_hardlink_and_command_permission` 失败 | 101 |
| `build.ps1 -Action fmt` | `cargo fmt --all -- --check` 通过 | 0 |
| `build.ps1 -Action clippy` | workspace/all-targets/`-D warnings` 通过，仅有既有 `proc-macro-error2` 未来兼容提示 | 0 |
| `build.ps1 -Action wasm-check` | 通过，同样只有既有未来兼容提示 | 0 |
| `git diff --check -- rust-app/src/server.rs rust-app/tests/http_contract.rs` | 通过 | 0 |

runtime 失败信息是 `无法启动 PowerShell` / `program not found`，入口在既有 `workspace::execute` 的命令夹具，不是本轮 server 改动。未付费 live 测试保持忽略。

单独的 `cargo test --locked --test http_contract -- --test-threads=1` 在补上脚本同等 MSVC 环境后为 11 通过、0 失败，退出码 0。

## 7. 未完成、风险和决策

- 新连续 Turn 写入口未挂载。计划仍是 path 提供 SessionId，body 提供 agent_id、expected_last_turn_id、message，header 提供幂等 key，返回具体 Turn/Task 和 replayed。状态码、最终 DTO 和 B 的应用错误映射等联合任务确定。本轮不声称它可用。
- health 的 schema 读取失败已不再伪装成 `ok:true/schema_version:0`，但没有稳定 HTTP 夹具覆盖已打开连接上的 pragma 失败。需要负责人决定是否接受代码核对，或后续提供可注入的存储故障点。
- 现有 `Event` 序列化失败分支不可由正常持久化事件触发。H10 用损坏事件 JSON 覆盖开流后读取失败。
- B 的领域错误还没有统一 HTTP 映射。本轮没有提前把这些错误改成 500 或 409。
- 全量测试受既有 PowerShell 命令夹具影响，不是本轮传输契约失败。负责人复核时可把该失败与 HTTP 契约分开看。
