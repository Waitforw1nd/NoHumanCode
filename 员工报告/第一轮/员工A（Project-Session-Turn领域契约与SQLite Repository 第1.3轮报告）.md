# 员工 A（Project/Session/Turn 领域契约与 SQLite Repository 第 1.3 轮报告）

日期：2026-09-22
依据：`审查记录/第一轮/员工A review 1.2`
结论：R1 至 R4 和本轮新发现已有代码与测试。R5、R6 明确延期，不冻结 Repository 契约，也不向员工 B 发放实现许可。

## R1 新旧幂等 replay

公开 API：`Store::idempotency_replay(key, request_hash) -> Result<Option<IdempotencyReplay>>`。

结果类型：

- `Turn(Turn)`：命中新表 `idempotency_records`。
- `LegacyRun(Run)`：只命中旧表 `idempotency`。旧 Run 不会被猜成 Turn。
- `None`：两个表都没有该 key。
- 同 key、不同摘要：两个表都返回冲突错误。

`idempotent_turn` 仍保留。它遇到 `LegacyRun` 时返回 `legacy replay requires legacy handling`，不再返回 `None`。

查询顺序是新表优先，再查旧表。旧空摘要仍视为兼容旧客户端；非空且不同的摘要是冲突。

测试：`store::tests::legacy_idempotency_replay_is_explicit`。

失败场景：旧 `create_run_with_idempotency` 的 key 用不同摘要重试，返回冲突，不创建第二个 Run。

## R2 schema 6 全量结构检查

`REQUIRED_COLUMNS` 覆盖 `projects`、`sessions`、`turns`、`agents`、`turn_tasks`、`turn_task_dependencies`、`idempotency_records`。

建表后再次检查这些列。`turn_tasks.legacy_task_id` 先查重复值，再要求唯一索引 `turn_tasks_legacy_task_id_unique`。缺列、重复 legacy id 或缺少唯一索引时，迁移在打开数据库时失败，不推进 `user_version`。

测试：`store::tests::partial_schema_6_table_stops_before_user_version`。

未实现：没有逐表构造缺外键和错误主键的全部组合。当前检查覆盖必需列和 legacy id 唯一索引。

## R3 legacy_task_id 唯一身份

新增唯一索引 `turn_tasks_legacy_task_id_unique`。重复值在迁移阶段停止。事件归属先按 TurnTask id 查找，未命中再按唯一 `legacy_task_id` 查找。

测试：`store::tests::event_task_must_belong_to_turn_or_session`。

失败场景：事件引用不存在的 task 时拒绝写入。

## R4 持久化文本和复合字段

新增：

- `secrets::validate_persisted_id`
- `secrets::safe_metadata_text`

旧 `create_run`、`create_run_with_idempotency`、Project、Session 写入都经过这两个入口。标题、名称、路径命中 bearer、authorization 或 token 前缀时拒绝写入，不改写成标识。

字段名继续先规范化。`authorizationHeader`、`APIKey`、`tokenValue` 的普通值整段替换为 `[redacted]`。`max_tokens` 不再被误判为凭据字段。

测试：

- `secrets::tests::persisted_values_redact_nested_and_non_sk_tokens`
- `store::tests::legacy_create_run_redacts_prompt_messages_and_errors`
- `store::tests::legacy_idempotency_replay_is_explicit` 中的敏感标题拒绝

## 本轮新发现

空 `tasks` 在 `commit_turn` 开始时拒绝。测试确认不会创建 Turn。

`task_dependencies` 对不存在的 Task 返回“执行任务不存在”，不再返回空列表。存在但没有依赖时才返回空列表。

测试：`store::tests::legacy_idempotency_replay_is_explicit`、`store::tests::dependencies_are_same_turn_and_acyclic`。

## R5 延期

接手：员工 A 先交付领域命令，员工 B 才能接 Engine，员工 C 最后接 HTTP。

前置依赖：R1 至 R4 已在本轮落地，但状态机和组合创建还没有代码。

计划：下一轮实现 `create_project_session_bundle`、统一输入校验和 `LifecycleStatus::can_transition`。

禁止调用：B/C 不得把 `insert_project`、`insert_session`、`insert_agent`、`update_turn_status`、`update_task_status` 暴露为 HTTP，也不得绕过它们直接写 SQL。

## R6 延期

接手：员工 A 先冻结 protocol v1 核心事件枚举；员工 B 再把 Engine 改到 Repository 命令；员工 C 接 HTTP/SSE；员工 D 做跨链路验收。

前置依赖：Repository 仍不是 Engine 的唯一写入路径。`Engine::start` 仍调用旧 `create_run`。

计划：A 下一轮定义核心事件枚举和兼容 `unknown:` 前缀。B/C 不在本轮开始实现。

禁止调用：B 不得继续新增对旧 `create_run` 的写入调用。C 不得把当前字符串 `kind` 当成已冻结事件协议。

## 门禁

通过：

- `pwsh -NoProfile -File rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action wasm-check`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action test`

结果：库测试 25 通过，对抗测试 4 通过，运行时测试 9 通过，protocol 测试 5 通过。`live` 仍忽略。

这台机器没有 `pwsh.exe`。完整测试时把 `powershell.exe` 以 `pwsh.exe` 放入 PATH 后，原有命令测试通过。这不是本轮存储改动引入的。
