# 员工 A（Project/Session/Turn 领域契约与 SQLite Repository 第 1.2 轮报告）

日期：2026-09-22
依据：`审查记录/第一轮/员工A review 1.1`
结论：已按 1.1 审查返工。Repository 仍未接入 Engine/HTTP，本轮不能声称 Project/Session/Turn 产品流程已经完成，也不能自行冻结契约。

## 本轮修复

### P0 旧创建路径不再绕过脱敏

新增唯一入口 `domain::safe_task_value`。它内部调用 `secrets::redact_persisted`。以下路径全部使用它：

- `Store::create_run`
- `Store::create_run_with_idempotency`
- `Store::commit_turn`
- `Store::save_task`
- `Store::recover`

回归测试通过旧 `create_run` 写入 prompt、messages、tool arguments、output 和 error，并直接查询 SQLite 原文。`sk-promptsecret`、`plain-provider-token`、`camel-token`、`xai-notaskey` 不落盘，普通文本仍保留。

### P1 依赖成为可写入的领域字段

`TurnTask.depends_on` 是 `Vec<TaskId>`。`commit_turn` 在同一事务中：

1. 插入全部 TurnTask；
2. 写入 `turn_task_dependencies`；
3. 拒绝自依赖、未知任务和跨回合依赖；
4. 检测循环依赖；
5. 与旧任务投影、幂等记录和初始事件一起提交。

`Store::task_dependencies` 对外读取依赖。`turn_tasks` 读取时回填 `depends_on`。旧 `TaskSpec.depends_on` 仍是显示名，只存在于兼容 JSON。

### P1 事件 task 归属

有 `turn_id` 时，`task_id` 必须是该回合的 TurnTask 或它的 `legacy_task_id`。只有 `session_id` 时，任务必须属于该会话的 legacy run。三者矛盾或不存在的任务会被拒绝。

### P1 幂等字段和旧投影绑定

传入 key 时，`turn.idempotency_key` 必须相同。不传 key 时，Turn 不能残留幂等 key。每个旧任务的 `run_id` 必须等于 `Session.legacy_run_id`，任务 ID 必须对应某个 `TurnTask.legacy_task_id`。

### P1 camelCase 脱敏

字段名先把大小写边界、短横线、点号和空格规范成下划线，再匹配 token、secret、key、authorization、credential。测试覆盖 `accessToken`、`modelToken`、`providerKey`、`authorizationHeader`、`clientSecret` 和嵌套数组。

这仍是启发式边界，不是绝对保证。无法可靠识别的任意 secret 文本不能据此宣称绝对安全。

### P1 schema 6 不完整表

迁移前检查 `projects`、`sessions`、`turns`、`turn_tasks` 的必要列。表不存在时继续创建。同名表缺列时停止，并返回明确错误，不推进 `user_version`。测试覆盖缺列的 `projects`。

## 仍未完成

- 生命周期状态机没有实现。B 不能把任意状态更新暴露为 HTTP。
- Project、Session、Agent 仍是独立插入，可能留下孤立对象。
- 路径、名称、ID、标题和幂等 key 还没有统一输入约束。
- 核心事件 `kind` 仍是字符串。
- Repository 没有接到 `Engine::start` 或 HTTP。

## 测试

通过：

- `pwsh -NoProfile -File rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action wasm-check`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action test`

结果：库测试 24 通过，对抗测试 4 通过，运行时测试 9 通过，protocol 测试 5 通过。`live` 仍忽略。

这台机器没有 `pwsh.exe`。完整测试时把 `powershell.exe` 以 `pwsh.exe` 放入 PATH 后，原有命令测试通过。这不是本轮存储改动引入的。
