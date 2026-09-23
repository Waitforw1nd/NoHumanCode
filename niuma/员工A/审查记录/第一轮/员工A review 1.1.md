# 员工 A Review — 第 1.1 轮

审查对象：niuma/员工A/提交报告/第一轮/员工A（Project-Session-Turn领域契约与SQLite Repository 第1.1轮报告）.md
审查日期：2026-09-22
审查人：项目总监
结论：CHANGES_REQUIRED

## 已确认修复

- legacy_run_id 现在从 sessions 表读取，且新旧幂等表使用同一旧 run ID。
- 新幂等 key 的同请求 replay 返回原 Turn。
- 旧 EventEnvelope 缺失 cursor 时会按 seq 补齐，伪造 cursor 会被拒绝。
- 新事件必须由数据库分配 seq，不能注入正数 seq/cursor。
- 旧任务和新回合的恢复状态与 status 事件已放入同一事务。
- Turn/Task/Agent 的基本 Project/Session 归属校验已增加。
- 事件和 Task JSON 现在经过统一 redact_persisted。
- 已重新运行实际门禁：
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action fmt
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action clippy
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action wasm-check
- 结果全部通过：20 个库测试、4 个 adversarial 测试、9 个 runtime 测试、5 个 protocol 测试；live 仍按设计忽略。

## 必须修复

### P1 — TurnTask 依赖关系表没有接入领域对象和提交入口

报告声称已经实现 turn_task_dependencies，但 TurnTask 仍没有 depends_on 字段，Repository::commit_turn 也没有调用 replace_dependencies。replace_dependencies 只能接收底层 Transaction；Store 没有公开封装，Engine 后续无法使用这个关系。

这会造成“数据库有表，但领域契约不能写入或读取依赖”的假完成状态。

要求：

1. 在 TurnTask 或明确的 TurnCommit 输入中携带 TaskId 依赖。
2. 让 commit_turn 在同一事务中写入依赖，拒绝自依赖、跨 Turn 依赖和循环依赖。
3. 在 Store 暴露读取依赖的安全 API，供 Engine/HTTP 使用。
4. 增加测试：正常依赖、跨 Turn、未知任务、自依赖和循环依赖。
5. 如果决定延期，必须删除“本轮已实现”的表述，并明确 B/C 不得使用它。

### P1 — 事件没有校验 task_id 与 turn/session 的归属

insert_event_tx 目前校验 session_id 存在、turn_id 属于 session，但没有校验 event.task_id 属于该 turn/session。调用者可以写入一个有效 turn_id 配一个无关 task_id，事件会被持久化并可能在旧 run 查询中丢失。

要求：

- 有 turn_id 时，校验 task_id 对应的 TurnTask 或 legacy task 属于该 turn；
- 有 session_id 但无 turn_id 时，校验 task 属于该 session 的 legacy run；
- 增加正例和错误组合测试。

### P1 — 幂等字段未绑定，兼容投影仍可错配

Store::commit_turn 接收 key，但没有验证 turn.idempotency_key 与传入 key 一致；legacy_tasks 的 run_id 也没有验证为 session.legacy_run_id。内部调用可写入互相矛盾的幂等和旧任务投影。

要求：

- key 存在时强制 turn.idempotency_key == key；没有 key 时 Turn 中也不能残留幂等 key；
- 所有 legacy task 的 run_id 必须等于当前 Session.legacy_run_id；
- 增加不一致输入的拒绝测试。

### P1 — redact_persisted 仍是启发式边界，存在 camelCase 和任意 token 漏洞

当前字段匹配将 accessToken 归一为 accesstoken，不会命中 access_token；modelToken、providerKey 等也可能绕过。文本规则只覆盖有限前缀和固定 assignment 名称，不能保证任意 provider/account token 不落盘。

要求：

- 对 camelCase、短横线、下划线字段统一规范化；
- 增加 accessToken、modelToken、providerKey、authorizationHeader 等测试；
- 对不能可靠识别的 secret-bearing 字段，采用显式拒绝或结构化安全 DTO，不要把通用字符串当作“已安全”；
- 测试必须检查 Task 的 prompt、messages、tool args、output、error 全部持久化路径。

### P1 — schema 6 部分 DDL 重试策略不完整

SCHEMA_6_SQL 对新表使用 CREATE TABLE IF NOT EXISTS。若崩溃后留下一个结构不完整但同名的表，重试会跳过建表，之后才在写入时失败。现有 partial migration 测试只覆盖完整 projects 表，不覆盖缺列/缺索引。

要求：

- 增加 schema shape 校验和补列/修复索引，或在检测到不兼容结构时明确停止并给出可恢复错误；
- 增加 projects/sessions/turns/turn_tasks 缺列的迁移测试。

## 可延期但必须记录

- 完整生命周期状态转换仍未实现；B 不能把任意状态更新暴露为 HTTP。
- Project/Session/Agent 独立插入仍可能留下孤立对象；C 接入创建 API 前需要组合事务。
- Project/path/ID/title/idempotency key 的统一输入约束仍缺失。
- 核心事件 kind 仍是字符串；保留兼容可以，但应在 protocol v1 中定义核心事件集合。
- Repository 尚未接入 Engine/HTTP；本轮不能声称已有完整 Project/Session/Turn 产品流程。

## 放行条件

A 需要补齐依赖写入/读取和事件 task 归属校验，并为幂等字段绑定、camelCase 脱敏、schema shape 重试增加测试。完成后再决定是否冻结契约。

在这些条件完成前，员工 B 可以做只读设计和威胁模型，但不要把当前 Repository 当作已冻结的 Engine 依赖接口。


### P0 — 旧 Engine 创建路径仍绕过凭据持久化边界

位置：NoManCode/rust-app/src/store.rs:130-169

create_run 和 create_run_with_idempotency 仍直接使用 serde_json::to_string(task) 写入 tasks。当前 Engine::start 会走这条旧路径，因此包含 prompt、messages、tool 参数、output 或 error 的首次任务可能绕过 redact_persisted。

这使得“事件和任务 JSON 不再直接保存凭据材料”仍不成立。save_task 和 commit_turn 已经脱敏，不足以覆盖首次创建路径。

要求：

- 抽出唯一的安全 Task 序列化函数，让 create_run、create_run_with_idempotency、commit_turn、save_task、recover 全部使用；
- 增加通过旧 Engine::start/create_run 路径写入敏感字段的回归测试；
- 测试必须覆盖 prompt、messages、tool arguments、output、error；
- 在这项修复完成前，凭据持久化 P0 仍未关闭。



## 解决思路与返工顺序

### 1. 先封住旧任务创建路径的 P0

新增唯一的安全序列化入口，例如 safe_task_value(task)，内部调用 secrets::redact_persisted。

以下路径必须全部使用同一个入口：

- Store::create_run
- Store::create_run_with_idempotency
- Store::commit_turn
- Store::save_task
- Store::recover

不要在各个调用点分别复制脱敏逻辑。先增加回归测试，直接通过旧 Engine::start/create_run 写入：

- prompt 中的 token；
- messages 中的 authorization/api_key；
- tool arguments 中的嵌套 secret；
- output 和 error 中的 bearer/token 文本。

测试应直接查询 SQLite 原始 tasks.value 和 events.data，确认明文不存在，同时确认正常代码内容没有被破坏。

### 2. 把依赖关系变成可用的领域契约

建议把 TurnTask 扩展为带 depends_on: Vec<TaskId> 的输入模型，或新增 TurnTaskInput。

commit_turn 必须在同一事务内按以下顺序处理：

1. 插入所有 TurnTask；
2. 校验每个依赖任务存在且属于同一 Turn；
3. 拒绝自依赖；
4. 对依赖图做环检测；
5. 写入 turn_task_dependencies；
6. 提交 Turn、旧 Task 投影和初始事件。

Store 需要公开：

- commit_turn_with_dependencies；
- dependencies(task_id)；
- 可选的 dependency_graph(turn_id)。

增加正常依赖、未知依赖、跨 Turn、自依赖和环依赖测试。Engine 后续只能使用这些稳定 TaskId API，不能继续把显示名当依赖身份。

### 3. 加强事件关系校验

insert_event_tx 在写入前必须验证：

- 有 turn_id 时，task_id 对应的 TurnTask 属于该 Turn；
- 有 session_id 但无 turn_id 时，task_id 属于该 Session 的 legacy run；
- session_id、turn_id、task_id 三者不能互相矛盾；
- 不允许写入不存在的 task 引用。

为有效组合和三种错误组合分别增加测试。

### 4. 绑定幂等字段和旧投影

Store::commit_turn 入口增加一致性检查：

- 传入 key 时，turn.idempotency_key 必须相同；
- 不传 key 时，turn.idempotency_key 必须为空；
- 所有 legacy_tasks 的 run_id 必须等于 Session.legacy_run_id；
- legacy task 的 ID、事件 task_id 和 TurnTask.legacy_task_id 必须对应。

旧 idempotency 表与新 idempotency_records 的查询顺序和 replay 行为写成测试，不能让旧 key 最后才以 UNIQUE 错误失败。

### 5. 补强脱敏字段规范化

敏感字段名先做统一规范化：

- 在大小写边界插入下划线；
- 将短横线、点号和空格转换为下划线；
- 再匹配 token、secret、key、authorization、credential 等词。

至少测试：

- accessToken；
- modelToken；
- providerKey；
- authorizationHeader；
- clientSecret；
- 嵌套数组中的同类字段。

对于无法可靠识别的 secret-bearing 字段，应拒绝持久化或改用显式安全 DTO；不能把启发式文本扫描当作绝对保证。

### 6. 修复 schema 6 部分迁移重试

迁移启动时不仅检查迁移标记，还要检查新表的列、主键、外键和索引。发现同名但结构不完整的表时：

- 能安全补列就补列；
- 不能安全修复就停止并返回明确的数据库迁移错误；
- 不要等到 Engine 写入时才暴露失败。

增加缺列、缺索引和部分表结构的迁移测试。

### 7. 放行顺序

A 完成上述修复并通过完整门禁后，我再冻结 Repository 契约。之后：

- B 才能基于稳定 Repository 实现 Engine、Approval 和 ToolCall；
- C 才能接 HTTP、SSE 和 CLI；
- D 负责最终跨模块安全验收。

在 A 修订完成前，不把当前 Repository 描述成“已接入 Project/Session/Turn 的完整后端流程”。

