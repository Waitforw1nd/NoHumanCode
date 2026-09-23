# 员工 A（Project/Session/Turn 领域契约与 SQLite Repository 第 1.1 轮报告）

日期：2026-09-22
依据：`审查记录/第一轮/员工A review`
结论：已按审查返工。Repository 仍未接入 Engine/HTTP，不能向员工 B 发放“契约已冻结”的放行结论。

## 本轮修复

### P0 事件和任务 JSON 不再直接保存凭据材料

`secrets::redact_persisted` 是后续 Engine、Approval 和 HTTP 应使用的统一边界。它递归处理对象、数组和字符串：

- 敏感字段名整值替换为 `[redacted]`，包括 `token`、`authorization`、`key`、`secret` 及 `_token`、`_key` 等后缀；
- 文本中的 `bearer`、`token=`、`api_key=` 和常见 token 前缀（`sk-`、`xai-`、`ntn_`、`ghp_` 等）也会替换；
- 不依赖单一的 `sk-` 规则。

`Store::event`、`save_task`、`commit_turn` 的旧任务 JSON，以及 Repository 的事件插入都会先经过该函数。

### P1 legacy_run_id

`idempotency_records.legacy_run_id` 改为从事务内的 `sessions.legacy_run_id` 读取，不再写入 `SessionId`。旧 `idempotency.run_id` 使用同一个值。

查询优先级：先查 `idempotency_records`。没有新记录时再查旧 `idempotency`。旧 key 命中同摘要时明确拒绝新建回合，避免落到 UNIQUE 错误；不同摘要返回冲突。

### P1 EventEnvelope cursor

反序列化缺少 `cursor` 时填 `seq.to_string()`。非空 `cursor` 必须等于 `seq` 的十进制字符串，否则拒绝。protocol 测试直接反序列化旧 JSON，断言 `cursor == "7"`，并拒绝伪造游标。

### P1 新事件 seq

新事件必须传入 `seq = 0`，由 SQLite `AUTOINCREMENT` 分配，再把 `cursor` 写成该 seq。调用者不能注入 seq 或伪造 cursor。`schema_version` 必须是当前事件版本。

### P1 恢复事务

旧任务和新回合的 `interrupted` 状态与对应 `status` 事件在同一个事务提交。事件写入失败时，状态更新回滚。

### P1 TurnTask 依赖

本轮实现了 `turn_task_dependencies`。依赖使用任务 ID，禁止自依赖和跨回合依赖。旧 `TaskSpec.depends_on` 仍是显示名，只存在于兼容 JSON，不是新依赖模型。

### P1 归属校验和幂等 replay

写入 Turn 时校验它属于该 Session 的 Project。写入 TurnTask 时校验 Turn 和 Agent 属于同一 Session。事件引用的 Session、Turn 也要匹配。

`Store::commit_turn` 对同 key、同摘要返回原来的 `Turn`，不再返回空成功。调用者不能把自己新生成的 ID 当成新对象。

## 明确延期

- 状态转换规则还没有完整状态机。当前只校验归属，不校验 `queued -> completed` 这类跳转是否合法。
- Project、Session、Agent 仍是独立插入。只有 Turn 提交是组合事务。孤立 Project/Session 的原子创建留到接入 Engine/HTTP 前。
- 路径、名称、ID 和幂等 key 还没有统一长度与控制字符校验。
- 核心事件 `kind` 仍是字符串，没有 namespace。
- 新 Repository 没有接到 `Engine::start` 或 HTTP。旧启动路径不会自动创建 Project、Session、Turn。

## 测试

通过：

- `pwsh -NoProfile -File rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action wasm-check`
- `pwsh -NoProfile -File rust-app/build.ps1 -Action test`

结果：库测试 20 通过，对抗测试 4 通过，运行时测试 9 通过，protocol 测试 5 通过。`live` 仍忽略。

新增覆盖：嵌套 JSON、非 `sk-` token、真实 legacy run id、replay 返回原 Turn、伪造 cursor 被拒绝。

这台机器没有 `pwsh.exe`。完整测试时把 `powershell.exe` 以 `pwsh.exe` 放入 PATH 后，原有命令测试通过。这不是本轮存储改动引入的。
