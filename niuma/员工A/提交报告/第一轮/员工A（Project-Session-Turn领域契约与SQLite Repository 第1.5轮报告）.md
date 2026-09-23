# 员工 A（Project/Session/Turn 领域契约与 SQLite Repository 第 1.5 轮报告）

日期：2026-09-22
依据：`审查记录/第一轮/员工A review 1.4`
结论：本轮收口了写入路径的双表幂等、旧 Run 标题和依赖替换顺序。R2 的真实主键/外键结构检查、R5、R6 仍未完成，不冻结契约，也不向 B/C 发放实现许可。

## 本轮修复

### R1 commit_turn 与 replay 共用双表判定

代码入口：`store.rs::classify_both`。

`Store::idempotency_replay` 和 `Store::commit_turn` 都调用它。任一侧摘要冲突都返回冲突。两侧同时命中时，新 Turn 的 `legacy_run_id` 必须等于旧 Run；否则返回“新旧幂等记录指向不同对象”。旧表单独命中时，`commit_turn` 拒绝再创建新回合。

测试：`store::tests::legacy_idempotency_replay_is_explicit`。

失败场景：旧 key 的摘要被改成不同值后，replay 返回错误，`runs` 行数保持 1，没有新增 Run。

### 旧 Run 标题

`commit_turn` 新建旧 Run 时，标题先经过 `secrets::safe_metadata_text("run_title", ...)`，不再直接写 `session.title`。

### 依赖替换顺序

`Repository::replace_dependencies` 先检查全部依赖是否存在、属于同一 Turn 且不是自依赖，然后再删除旧依赖并插入新依赖。非法依赖不会先清空原依赖。

### Repository 空 Turn

`Repository::commit_turn` 改为 `pub(crate)`。外部不能绕过 `Store::commit_turn` 直接提交空 Turn。

## 仍未关闭

### R2

仍只检查七张新表的必需列和 `legacy_task_id` 唯一索引名称。没有核对主键顺序、外键目标和索引 `unique=1`。R2 未关闭。

### R3

事件归属仍是两步查找，但比较的是 `turn_id`。同一 Turn 内两个不同 Task 同时命中时，还不能按行身份报歧义。历史 `task.id == other.legacy_task_id` 的迁移检测也未完成。

### R4

`Repository::insert_project`、`insert_session`、`insert_agent` 仍是公开裸写入口。Agent 文本、Turn/Task 标识和嵌入标题中间的 token 扫描还没有全部收口。

### R5

延期。A 先交付组合创建、统一输入校验和状态转移；B 再接 Engine；C 最后接 HTTP。

禁止：B/C 不得把 `insert_project`、`insert_session`、`insert_agent`、`update_turn_status`、`update_task_status` 暴露为 HTTP。

### R6

延期。Engine 仍可能使用旧 `create_run`。核心事件 kind 仍是字符串。

禁止：B 不得新增旧 `create_run` 写入调用。C 不得把字符串 `kind` 当成已冻结事件协议。

## 门禁

通过：

- `pwsh -NoProfile -File rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action wasm-check`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action test`

结果：库测试 25 通过，对抗测试 4 通过，运行时测试 9 通过，protocol 测试 5 通过。`live` 仍忽略。

这台机器没有 `pwsh.exe`。完整测试时把 `powershell.exe` 以 `pwsh.exe` 放入 PATH 后，原有命令测试通过。这不是本轮存储改动引入的。
