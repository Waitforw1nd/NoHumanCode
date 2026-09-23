# 员工 A Review — 第 1.3 轮

审查对象：niuma/员工A/提交报告/第一轮/员工A（Project-Session-Turn领域契约与SQLite Repository 第1.3轮报告）.md
审查日期：2026-09-22
审查人：项目总监
结论：CHANGES_REQUIRED

## 本轮实际门禁

已实际运行：

- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action wasm-check`

结果通过：25 个库测试、4 个 adversarial 测试、9 个 runtime 测试、5 个 protocol 测试；live 测试仍按设计忽略。门禁通过只能说明当前测试集通过，不能替代下面的契约审查。

## 已确认完成

- 新增 `idempotency_replay`，旧 Run 不再被伪造为 Turn。
- `task_dependencies` 对不存在任务返回错误，空 Turn 在 `commit_turn` 入口拒绝。
- schema 6 已覆盖七张新表的必需列，并增加 legacy task 唯一索引名称检查。
- 旧 Run 创建路径使用 `insert_legacy_run`，Task JSON 继续使用统一安全序列化。
- R5、R6 仍明确延期，报告没有把它们误报为已完成。

## 必须修复

### P1 — R3 报告与实际事件归属实现不一致

报告声称事件归属“先按 TurnTask id 查找，未命中再按唯一 legacy_task_id 查找”。实际 `NoManCode/rust-app/src/repository.rs` 的 `insert_event_tx` 仍执行：

```sql
SELECT turn_id FROM turn_tasks WHERE id=?1 OR legacy_task_id=?1
```

这不是两步确定性解析。即使 `legacy_task_id` 唯一，仍可能出现一个任务的 `id` 等于另一个任务的 `legacy_task_id`；此时 `query_row` 会取决于 SQLite 返回顺序，事件可能归属错误。现有 `event_task_must_belong_to_turn_or_session` 只覆盖不存在任务，没有覆盖 ID/legacy ID 碰撞。

**具体修复**

1. 先按 `turn_tasks.id` 精确查询；命中后只接受该任务。
2. 未命中时再按 `legacy_task_id` 查询；查询必须确认最多一行。
3. 两步都命中不同任务时返回明确的“任务引用歧义”错误；不能选择第一行。
4. 为新 ID 命中、legacy ID 命中、双命中不同任务、重复 legacy ID 迁移失败分别增加测试。
5. 报告中的“已实现”必须以这些测试和对应代码位置为证据。

### P1 — R2 只检查列，仍未检查 schema 结构

`REQUIRED_COLUMNS` 已扩展到七张表，但 `verify_or_reject_partial_tables` 仍只读取 `PRAGMA table_info` 的列名，并通过 `sqlite_master` 中是否存在指定名称判断唯一索引。它没有验证：

- 主键列和复合主键；
- 外键目标与列；
- 索引是否真的为 UNIQUE；
- `turn_tasks_legacy_task_id_unique` 是否真的覆盖 `legacy_task_id`。

一个同名普通索引、错误主键或缺失外键的残缺数据库仍可能通过迁移。报告已经承认“没有逐表构造缺外键和错误主键”，因此 R2 不能关闭。

**具体修复**

1. 为七张表定义结构元数据：列、NOT NULL、主键、外键、唯一索引和关键普通索引。
2. 迁移启动时使用 `PRAGMA table_info`、`PRAGMA foreign_key_list`、`PRAGMA index_list`、`PRAGMA index_info` 全量核对。
3. 唯一索引必须核对 `unique=1` 且列顺序正确，不能只核对名字。
4. 缺结构时在迁移阶段返回表名和缺失项；任何检查失败都不能推进 migration marker 或 `user_version`。
5. 为每张新表至少添加缺列、错误主键、缺外键、缺唯一/关键索引的失败测试。

### P1 — R4 的持久化安全边界仍有可达绕过

#### 1. `commit_turn` 的旧 Run 投影仍直接写标题

`NoManCode/rust-app/src/store.rs::commit_turn` 在 Run 不存在时直接执行 `INSERT INTO runs(... title ...)`，没有调用 `insert_legacy_run` 或 `safe_metadata_text`。因此只要数据库中的 Session 记录来自旧数据或低层 Repository，Session title 就能绕过新安全入口写入旧 Run。

修复：统一使用事务内的 `insert_legacy_run`，或抽出接受 `&Session` 的安全 Run 投影函数，并在同一入口验证 Run ID、标题、kind。

#### 2. Agent 文本和标识没有使用统一校验

`Store::insert_agent` 仍只调用旧的 `reject_record_secrets`，没有验证 `agent.id`、`agent.session_id`、`display_name`、`role` 的长度、控制字符和敏感内容。`Repository::insert_agent`、`insert_session`、`insert_project` 仍是公开的裸写入口，也能绕过 Store 层的校验。

修复：将安全校验放到所有可达的持久化边界；底层 `*_tx` 只保留为私有事务函数。至少覆盖 Project、Session、Agent、Run、Turn、TurnTask、legacy IDs、幂等 key 和 request hash。

#### 3. Turn/Task/幂等字段没有统一 ID 校验

`commit_turn` 对 key 和 request hash 仍使用窄的 `reject_record_secrets`，没有复用 `validate_persisted_id`；`insert_turn_tx`、`insert_turn_task_tx` 也没有验证 TurnId、ProjectId、SessionId、AgentId、TaskId、legacy_task_id。控制字符、超长值或部分 token 形式仍可能直接进入结构化表和 API 返回。

修复：在 Turn/Task 插入事务的最前面统一验证所有标识和 key；为长值、控制字符、token 前后缀和正常中文标题/路径分别增加测试。

#### 4. `APIKey` 的测试存在同值掩盖，不能证明字段已脱敏

`normalize_field("APIKey")` 会得到类似 `a_p_i_key`，不匹配 `api_key`/`apikey`。当前测试把 `APIKey`、`tokenValue` 和重复的 `authorizationHeader` 都设成同一个 `plain-secret`；即使只有 `tokenValue` 被脱敏，断言也会通过，无法证明 `APIKey` 被处理。

修复：

- 对 acronym 做专门规范化，或显式加入 `a_p_i_key` 等等价形式；
- 使用互不相同的值测试 `APIKey-only`、`tokenValue-only`、`authorizationHeader-only`；
- 删除重复 JSON key，避免后者覆盖前者；
- 增加原始 JSON、SQLite 值和 API 回读三层断言。

#### 5. 普通文本中的 token 仍可能绕过 `safe_metadata_text`

`sensitive_text` 只在 `token_end(value, 0)` 检查 token 前缀，不能发现字符串中间的 `hello sk-secret`；`safe_metadata_text` 也没有复用完整的 `redact_text` 扫描。标题/名称中的非 Bearer token 因此可能落盘。

修复：对元数据字段采用“命中任意 token/authorization/secret 规则即拒绝”的完整扫描；不能可靠判断时拒绝写入。增加 `normal title + embedded sk-token`、`authorizationHeader: plain-value`、`APIKey: unique-value` 等测试。

### P1 — R1 虽有新 API，但双表一致性和调用迁移证据不足

`idempotency_replay` 的新旧查询顺序已实现，但当前测试主要覆盖“旧表单独命中”。没有证明以下数据异常下的稳定行为：

- 新旧表同 key、同摘要；
- 新表同 key 不同摘要而旧表有另一条记录；
- 旧表 key 指向不存在 Run；
- Engine/HTTP 是否已经改用新 `idempotency_replay`，而不是继续调用旧包装器。

**具体修复**

1. 明确新表优先还是双表冲突优先，并写成测试和文档。
2. 为上述四种双表/损坏数据情况增加测试，确保无副作用、不创建第二个 Run/Turn。
3. 全仓搜索旧 `idempotent_turn` 调用；新写请求必须使用显式 replay 类型，旧包装器只能留在兼容读取路径。
4. 旧 Run 缺失时返回明确的数据损坏错误，不得被解释为“没有 key”。

## R5/R6 处理意见

报告将 R5、R6 标记为延期是诚实的，但它们仍是发布阻塞项：

- R5 必须先由 A 提交组合创建、统一输入校验和合法状态转移命令，B 才能接 Engine，C 才能接 HTTP 写接口；
- R6 必须先冻结核心事件 kind/namespace，再将 Engine 的新写入路径迁移到 Project/Session/Turn/Task，最后由 C 接 SSE/HTTP、D 做跨链路验收。

在这些接口完成前，B/C 只能做只读设计和适配准备，不能把分立 `insert_*`、任意状态更新或旧 `create_run` 作为产品写入 API。

## 下一轮放行条件

1. 先修正 R3 的事件归属实现和碰撞测试。
2. 补齐 R2 的主键、外键、唯一索引和关键索引结构校验。
3. 统一所有 Project/Session/Agent/Run/Turn/Task/幂等字段的安全边界，修复 `APIKey` 测试掩盖和嵌入式 token 漏洞。
4. 完成 R1 双表异常场景测试，并证明 Engine/HTTP 写路径已使用统一 replay API。
5. 再运行 fmt、test、clippy、wasm-check，并在报告中逐项引用测试名称与代码入口。
6. R5/R6 仍未完成时，不得声称 Repository 已冻结或向 B/C 发放实现放行。

当前维持 `CHANGES_REQUIRED`。

## 补充复核：本轮代码仍存在的遗漏

### P1 — 幂等双表命中时提前返回，旧表冲突被静默忽略

`Store::idempotency_replay` 命中新 `idempotency_records` 后立即返回 `Turn`，没有继续读取旧 `idempotency`；`commit_turn` 的 `classify_idempotency` 也在新表命中后直接返回。若同一 key 在两张表中存在但 run/turn 或摘要不一致，当前实现会静默选择新表，无法发现数据不一致。

**修复要求**

- 查询两个表后再统一分类，不能在命中新表时提前返回；
- 任一侧摘要冲突都返回 Conflict；
- 两侧同摘要时校验旧 Run 与新 Turn 的兼容绑定；无法证明一致时返回数据不一致错误；
- 增加“双表同 key 同摘要”“双表同 key 不同摘要”“新 Turn 与旧 Run 不同绑定”三组测试，并确认无新对象写入。

### P1 — 敏感字段规则回归：`_password` 后缀被删除

`secrets.rs::sensitive_field` 当前不再包含 `_password` 后缀。`databasePassword`、`adminPassword` 等字段经过规范化后会落为 `database_password`、`admin_password`，但不再命中敏感字段规则，字段值可能明文保存。这个回归与本轮声称“复合字段已覆盖”矛盾。

**修复要求**

- 恢复 `_password`/`password` 及大小写、短横线、点号、空格等等价形式；
- 对 `APIKey`、`authorizationHeader`、`tokenValue`、`clientSecret`、`databasePassword` 分别使用不同的唯一值测试；
- 删除重复 JSON key，避免后者覆盖前者导致假阳性；
- 测试原始 JSON、SQLite 原文和 API 回读均不能出现明文。

### P1 — 新 Task ID 与别的 legacy_task_id 仍可碰撞

唯一索引只约束 `legacy_task_id` 列本身，没有阻止 `turn_tasks.id` 等于另一行的 `legacy_task_id`。这正是事件归属使用 `id OR legacy_task_id` 时产生歧义的来源。

**修复要求**

提交 TurnTask 时，在同一事务内检查新 ID 与全局 legacy ID 的交叉碰撞；事件归属采用精确两步查找并对双命中显式报错。增加 `task.id == other.legacy_task_id` 的提交和事件测试。

### P2 — 公开 Repository 写入口仍绕过 Store 安全边界

`Repository::insert_project`、`insert_session`、`insert_agent` 是公开方法，调用者可以直接传入未经过 `validate_persisted_id`/`safe_metadata_text` 的值。若这些方法是内部事务实现，应改为私有或 `pub(crate)`；若确实需要公开，必须在 Repository 层重复执行统一 DTO 校验，不能依赖调用方自觉走 Store。

## 修订后的放行门槛

R1 必须先解决双表一致性；R2 必须检查真实 PK/FK/UNIQUE 结构；R3 必须完成两步事件归属和交叉碰撞拒绝；R4 必须修复 `_password` 回归、APIKey 测试假阳性、Agent/Turn/Task/幂等字段直写。完成并补齐失败测试前，结论保持 `CHANGES_REQUIRED`。
