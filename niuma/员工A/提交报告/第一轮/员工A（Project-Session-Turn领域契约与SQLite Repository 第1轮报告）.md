# 员工 A（Project/Session/Turn 领域契约与 SQLite Repository 第 1 轮报告）

日期：2026-09-22

## 目标和范围

这次只做 Project、Session、Turn 的领域契约和 SQLite Repository。Session 类型继续只由协议字段 `SessionKind` 决定，不看成员显示名。旧 `runs` / `tasks` JSON 仍可读取，但不再作为新代码的长期领域模型。

没有改 `engine.rs`、`server.rs`、web 或旧 Node 系统，也没有把新 Repository 接到 HTTP。

## 修改的文件

- `rust-app/crates/protocol/src/lib.rs`
- `rust-app/crates/protocol/Cargo.toml`：`serde_json` 改为正式依赖
- `rust-app/src/domain.rs`
- `rust-app/src/repository.rs`：新增
- `rust-app/src/store.rs`
- `rust-app/src/lib.rs`

## 领域对象

新增稳定 ID：`ProjectId`、`SessionId`、`TurnId`、`AgentId`、`TaskId`、`EventId`。它们都是不透明字符串。

新增对象：

- `Project`：本机目录，身份是项目 ID，不是路径文本。
- `Session`：绑定一个项目；`kind` 单独保存。`legacy_run_id` 对应旧 `runs.id`。
- `Turn`：一次请求和恢复单位，带 `LifecycleStatus`、请求摘要和可选幂等 key。
- `Agent`：稳定 ID 与可改的 `display_name` 分开。显示名即使是 `chat-session`，也不决定会话类型。
- `TurnTask`：依赖身份使用任务 ID；`legacy_task_id` 对应旧 `tasks` 行。

`LifecycleStatus` 包含 `queued`、`running`、`interrupted`、`failed`、`completed` 等状态。中断不会写成 `completed`。

## API / Event / Protocol

没有新增 HTTP 路由。旧 `Run` / `Task` 响应形状不变。健康检查的 `schema_version` 会变为 6。

事件 envelope 版本为 1，字段是：

`schema_version`、`seq`、`cursor`、`session_id`、`turn_id`、`task_id`、`kind`、`data`、`timestamp`

`seq` 是单调游标，`cursor` 是它的十进制字符串。旧事件仍用 `at`；读取时映射为 `timestamp`，写出时继续保留 `at`，所以现有 SSE 不用改。`kind` 仍是字符串。

## Schema / Migration

`SCHEMA_VERSION` 从 5 升到 6。迁移标记是 `project-session-turn-repository`。

同一事务内完成建表、给空游标补 `cursor = seq`、写迁移标记和推进 `user_version`。失败会回滚，版本号不会提前推进。是否迁移看标记而不是版本号，因此中断后再次打开会补完，且不会重复写标记。

新增表：`projects`、`sessions`、`turns`、`agents`、`turn_tasks`、`idempotency_records`。`events` 增加 `schema_version`、`cursor`、`session_id`、`turn_id`。

## 权限和凭据

新表和事件不保存 API Key、DPAPI 明文或 New API token。`secrets` 表仍只存 DPAPI 密文。标题、路径、请求摘要和幂等 key 若包含 `sk-`、`dpapi` 或 `bearer ` 会被拒绝。

## 兼容策略

旧 `runs` / `tasks` JSON 按原样读取。schema 5 事件只补空游标，`schema_version` 保持 0，不补造 `session_id`。旧 HTTP 仍通过 `runs` / `tasks` 投影工作。新的持久入口是 `Store::commit_turn`；旧 `Engine::start` 仍只写旧表。

## 失败、取消和恢复

同 key、同请求摘要返回原来的 Turn。同 key、不同摘要返回「同一个 Idempotency-Key 不能用于不同请求」，不创建新对象。Turn、任务、旧 run/task、两张幂等表和起始状态事件在同一个事务提交；失败不会留下孤立 run。

`recover()` 把 `queued` / `running` 的旧任务和新回合改成 `interrupted`，并写 `status` 事件。再次启动不会把已中断对象重复改写，也不会伪造 `completed`。

## 测试命令和结果

通过：

- `pwsh -NoProfile -File rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action wasm-check`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action test`

测试结果：库测试 18 通过，对抗测试 4 通过，运行时测试 9 通过，protocol 测试 5 通过。`live` 仍忽略。覆盖了 schema 5 到 6、中断后重试、身份稳定、幂等冲突、事务回滚、游标单调和旧 JSON 读取。

这台机器没有 `pwsh.exe`。不补 PATH 时，原有测试 `replace_existing_file_preserves_hardlink_and_command_permission` 会报「无法启动 PowerShell」。把 `powershell.exe` 以 `pwsh.exe` 放入测试进程 PATH 后通过。这不是本次存储改动引入的。

## 未完成事项与风险

- 新 Repository 还没有接到 HTTP 或 Engine；旧启动路径不会自动创建 Project、Session、Turn。
- 旧任务依赖仍使用显示名，尚未迁移成稳定任务 ID。
- 没有把历史 run 批量回填成 Session。
- SQLite 的部分 DDL 不能随外层事务完全撤销。标记和 `user_version` 可以回滚；极端崩溃后可能留下空表，但重试是幂等的。
