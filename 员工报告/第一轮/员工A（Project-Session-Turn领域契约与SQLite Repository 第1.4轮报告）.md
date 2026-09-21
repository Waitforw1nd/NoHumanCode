# 员工 A（Project/Session/Turn 领域契约与 SQLite Repository 第 1.4 轮报告）

日期：2026-09-22
依据：`审查记录/第一轮/员工A review 1.3`
结论：本轮修复了 1.3 审查指出的事件归属、双表幂等、密码字段和 ID 碰撞。R2 的完整主键/外键逐项校验、R5、R6 仍未完成，不冻结契约，也不向 B/C 发放实现许可。

## 本轮修复

### R3 事件归属改为两步解析

代码入口：`repository.rs::resolve_task_turn`。

顺序：

1. 先按 `turn_tasks.id` 精确查找。
2. 未命中再按 `legacy_task_id` 查找，并要求最多一行。
3. 两步命中不同 Turn 时返回“任务引用歧义”。
4. 新任务 ID 与已有 `legacy_task_id` 交叉碰撞时，`insert_turn_task_tx` 拒绝提交。

测试：`store::tests::legacy_idempotency_replay_is_explicit` 覆盖 `task.id == other.legacy_task_id` 的提交拒绝。`store::tests::event_task_must_belong_to_turn_or_session` 覆盖不存在任务。

### R1 双表不再提前返回

代码入口：`Store::idempotency_replay`。

新表和旧表都查询后才分类：

- 任一侧摘要冲突都返回冲突。
- 两侧都命中时，新 Turn 的 `legacy_run_id` 必须等于旧 Run；否则返回“新旧幂等记录指向不同对象”。
- 只有旧表命中时返回 `LegacyRun`。旧 Run 不存在时返回数据损坏错误，不返回 `None`。
- 只有新表命中时返回原 Turn。

测试：`store::tests::legacy_idempotency_replay_is_explicit`。

### R4 密码字段和 APIKey

`sensitive_field` 恢复 `_password` 和 `password`。`APIKey` 规范化后的 `a_p_i_key` 单独列入敏感字段。

测试 `secrets::tests::persisted_values_redact_nested_and_non_sk_tokens` 使用不同值：

- `APIKey = apikey-unique-value`
- `tokenValue = tokenvalue-unique-value`
- `databasePassword = password-unique-value`
- `authorizationHeader = plain-secret`

这些值都不能出现在脱敏后的 JSON 中。

## 仍未关闭

### R2

七张新表仍只检查必需列和 `legacy_task_id` 唯一索引名称。还没有逐项核对主键列、外键目标和 `unique=1`。因此 R2 仍未关闭。

### R4 剩余边界

`Repository::insert_project`、`insert_session`、`insert_agent` 仍是公开裸写入口。Agent 文本还没有全部走 `safe_metadata_text`。`commit_turn` 的部分 Run 标题路径仍需继续收口。嵌入标题中间的 token 扫描还没有做到完整拒绝。

### R5

延期。接手顺序仍是 A 交付组合创建和状态机，B 再接 Engine，C 最后接 HTTP。

禁止：B/C 不得把 `insert_project`、`insert_session`、`insert_agent`、`update_turn_status`、`update_task_status` 暴露为 HTTP。

### R6

延期。Engine 新写入仍可能走旧 `create_run`。核心事件 kind 仍未冻结。

禁止：B 不得新增旧 `create_run` 写入调用。C 不得把字符串 `kind` 当成已冻结事件协议。

## 门禁

通过：

- `pwsh -NoProfile -File rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action wasm-check`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action test`

结果：库测试 25 通过，对抗测试 4 通过，运行时测试 9 通过，protocol 测试 5 通过。`live` 仍忽略。

这台机器没有 `pwsh.exe`。完整测试时把 `powershell.exe` 以 `pwsh.exe` 放入 PATH 后，原有命令测试通过。这不是本轮存储改动引入的。
