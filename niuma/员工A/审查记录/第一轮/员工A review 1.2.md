# 员工 A Review — 第 1.2 轮

审查对象：niuma/员工A/提交报告/第一轮/员工A（Project-Session-Turn领域契约与SQLite Repository 第1.2轮报告）.md
审查日期：2026-09-22
审查人：项目总监
结论：CHANGES_REQUIRED

## 已确认通过

- create_run、create_run_with_idempotency、commit_turn、save_task、recover 均经过 safe_task_value。
- TurnTask.depends_on 已进入领域对象，并在 commit_turn 同事务写入 turn_task_dependencies。
- 已增加自依赖、未知任务、跨 Turn 和环依赖校验。
- 事件 task_id 与 turn/session 归属校验已增加。
- 幂等 key 与 Turn.idempotency_key、旧 task.run_id 已做一致性校验。
- EventEnvelope cursor、camelCase 常见字段脱敏和部分表结构检测已有测试。
- 已运行实际门禁：
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action fmt
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action clippy
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action wasm-check
- 本地结果全部通过：24 个库测试、4 个 adversarial 测试、9 个 runtime 测试、5 个 protocol 测试；live 测试仍忽略。

## 必须修复

### P1 — 新旧幂等查询仍不统一

Repository::legacy_idempotency 已存在，但 Store::idempotent_turn 仍只查询 idempotency_records。旧 Engine 的 create_run_with_idempotency 只写旧 idempotency 表，因此同一个旧 key 通过 idempotent_turn 查询会得到 None。

要求：

- 统一 idempotent_turn 的新旧表查询顺序；
- 同 key 同摘要返回兼容结果；
- 同 key 不同摘要统一返回冲突；
- 增加旧 create_run_with_idempotency → idempotent_turn 的回归测试；
- 说明旧 Run 如何映射为 Turn，若无法返回 Turn，必须返回明确的 legacy replay 类型，而不是 None。

### P1 — schema shape 检查没有覆盖全部新表

REQUIRED_COLUMNS 当前只覆盖 projects、sessions、turns、turn_tasks，没有覆盖：

- agents；
- turn_task_dependencies；
- idempotency_records。

如果这些表在崩溃后以缺列结构存在，CREATE TABLE IF NOT EXISTS 会跳过，迁移成功后才在运行时失败。

要求：

- 把所有 schema 6 新表纳入结构检查；
- 检查必要列、主键、外键和关键索引；
- 为每个新表增加缺列/缺结构迁移测试；
- 结构不兼容时必须在启动迁移阶段明确失败。

### P1 — legacy_task_id 没有唯一约束

turn_tasks.legacy_task_id 没有 UNIQUE 约束。事件归属使用：

SELECT turn_id FROM turn_tasks WHERE id=? OR legacy_task_id=?

重复 legacy_task_id 会造成多行结果或不确定归属，破坏事件审计和旧投影映射。

要求：

- 增加 UNIQUE 约束或唯一索引；
- 迁移时检测重复值并明确失败；
- 事件归属测试覆盖重复 ID。

### P1 — Run 标题和标识仍可能绕过持久化安全边界

safe_task_value 只覆盖 Task JSON。create_run 和 commit_turn 仍把 run.title/session.title 以及部分 run/task 标识直接写入数据库。用户输入的标题可能包含 account token 或 provider key，随后会从历史列表和 API 返回。

要求：

- 对所有持久化用户文本统一使用安全 DTO、显式拒绝或脱敏；
- 至少覆盖 Run title、Session title、Project name/path、Task id/run_id；
- 增加旧 create_run、commit_turn 和历史 runs 查询的原文测试；
- 不要把“Task JSON 已脱敏”当作整个 persistence boundary 已完成。

### P1 — 敏感字段识别仍不够完整

当前 authorizationHeader 只有在值本身包含 Bearer 时才会被擦除；普通 authorizationHeader 值、APIKey、tokenValue 等变体仍可能落盘。

要求：

- 对包含 authorization、token、secret、credential、api/key 的复合字段统一按敏感字段处理；
- 增加普通文本值的正向脱敏测试；
- 对无法可靠识别的字段使用显式安全 DTO 或拒绝持久化。

### P1 — 空 Turn 的提交行为不明确

commit_turn 允许 tasks 为空，但随后创建的 opening event 使用 turn.id 作为 task_id，insert_event_tx 会因为不存在对应 TurnTask 而拒绝。

要求：

- 明确拒绝空 tasks；或
- 支持合法的 turn-level event，不伪造 task_id；
- 增加空 Turn 测试。

## 可延期但必须记录

- 生命周期状态转换仍未实现；B 不得把任意状态更新直接暴露为 HTTP。
- Project、Session、Agent 仍可独立插入；C 接入创建接口前需要组合事务。
- 路径、名称、ID、标题和幂等 key 的统一输入约束仍未完成。
- Repository 尚未接入 Engine/HTTP；当前不能声称完整 Project/Session/Turn 产品流程已完成。

## 建议修复顺序

1. 先统一旧/新幂等查询，并拒绝或支持空 Turn。
2. 补全 schema shape 检查和 legacy_task_id 唯一性。
3. 把 Run/Session/Project/Task 的用户文本纳入统一持久化安全边界。
4. 收紧复合敏感字段识别并补普通 authorizationHeader 测试。
5. 重新运行完整门禁并更新报告。

在以上问题解决前，不冻结 Repository 契约，也不向员工 B 发放正式实现许可。

## 连续两轮未关闭问题：具体返工方案

以下问题在第 1.1 轮已经提出，第 1.2 轮仍未达到关闭条件。本节不是重复结论，而是给员工 A 的直接返工设计、实现边界和验收标准。完成前继续保持 `CHANGES_REQUIRED`，不得冻结 Repository 契约。

### R1 — 新旧幂等查询与 replay 类型不一致（1.1 → 1.2 仍未关闭）

**问题根因**

`Store::commit_turn` 已经能识别旧 `idempotency` 表，但 `Store::idempotent_turn` 仍只返回新 `idempotency_records` 的 `Turn`。旧 `create_run_with_idempotency` 写入的 key 经过重试会得到 `None`，调用方可能再次创建对象，或者最后才撞到 UNIQUE 错误。

**具体实现方案**

1. 在 Store 层新增统一查询入口，例如 `idempotency_replay(key, request_hash)`，先查 `idempotency_records`，再查旧 `idempotency`，两个表都必须执行“同摘要命中、不同摘要冲突”的判定。
2. 不要把旧 Run 伪造为新 Turn。因为旧表只有 `run_id`，不一定有 `project_id/session_id/turn_id`，无法安全构造完整 Turn。定义显式结果类型：
   - `Turn(Turn)`：命中新表；
   - `LegacyRun(Run)`：只命中旧表；
   - `Conflict`：同 key 不同摘要。
3. 保留旧 `idempotent_turn` 作为兼容包装时，命中 `LegacyRun` 必须返回明确错误 `legacy replay requires legacy handling`，不能返回 `None`；新 Engine/HTTP 使用统一枚举 API。
4. 如果产品决定把旧 Run 升级成 Turn，必须另写一次性导入函数，显式要求 `project_id/session_id/agent_id`，导入成功后再写 `idempotency_records`；不能在普通重试路径猜测这些字段。
5. 查询和“首次写入”必须使用同一摘要规范：空旧摘要只能表示兼容旧客户端，不得把非空不同摘要当作同请求。

**必须增加的测试**

- 旧 `create_run_with_idempotency` → 统一 replay 返回 `LegacyRun`；
- 新 `commit_turn` → 统一 replay 返回原 `Turn`；
- 新旧表同 key、同摘要时有确定优先级且不重复创建；
- 新旧表同 key、不同摘要都返回冲突；
- replay 后再次调用 `commit_turn` 不产生第二个 Run/Turn。

**验收条件**

代码中不得再存在“只查新表的公开幂等查询”；所有入口对旧 key 都返回明确结果或明确错误，不能返回 `None` 掩盖旧记录。

### R2 — schema 6 结构校验只覆盖部分表（1.1 → 1.2 仍未关闭）

**问题根因**

`REQUIRED_COLUMNS` 只覆盖 `projects/sessions/turns/turn_tasks`。`agents`、`turn_task_dependencies`、`idempotency_records` 如果以同名残缺表留下，`CREATE TABLE IF NOT EXISTS` 会跳过，迁移仍可能推进到 version 6，错误延迟到写入阶段。

**具体实现方案**

1. 把 schema 6 的期望结构集中定义为一份元数据，至少包含每张表的：必需列、主键列、外键关系、唯一约束/索引。
2. `verify_or_reject_partial_tables` 必须覆盖全部 7 张新表：`projects`、`sessions`、`turns`、`agents`、`turn_tasks`、`turn_task_dependencies`、`idempotency_records`。
3. 检查不能只用 `table_info`：
   - `PRAGMA table_info` 检查列和 NOT NULL；
   - `PRAGMA foreign_key_list` 检查引用目标；
   - `PRAGMA index_list/index_info` 检查唯一索引和关键索引；
   - 对依赖表检查复合主键 `(task_id, depends_on_task_id)`。
4. 对“表存在但缺结构”统一在迁移阶段返回包含表名、缺失结构和修复建议的错误；不要自动猜测性改写已有数据。
5. 将 `legacy_task_id` 的唯一索引也纳入 schema shape 校验，避免表结构检查和运行时约束分离。
6. 所有检查通过后才写 `schema_migrations` 和 `user_version=6`；任意检查失败都要验证事务回滚，不能留下半迁移状态。

**必须增加的测试**

对上述每张表分别构造：缺一列、缺外键、缺唯一索引/关键索引、错误主键；验证启动迁移立即失败、错误包含对象名、`user_version` 和迁移标记未推进。另加完整 schema 的重试测试。

**验收条件**

删除或破坏任意 schema 6 新表的关键结构，都必须在打开数据库时失败，而不是等到 Engine/HTTP 首次写入才失败。

### R3 — `legacy_task_id` 未形成唯一身份（1.1 → 1.2 仍未关闭）

**问题根因**

`turn_tasks.legacy_task_id` 当前没有 UNIQUE。事件归属通过 `id=? OR legacy_task_id=?` 查询，重复值或 ID/legacy ID 碰撞时可能取到不确定的第一行，审计链和旧投影映射会失真。

**具体实现方案**

1. 为 `turn_tasks.legacy_task_id` 增加唯一索引，例如 `turn_tasks_legacy_task_id_unique`；将该索引纳入 schema 6 shape 检查。
2. 创建唯一索引前先执行重复检测，返回重复值和涉及的 Turn/Task；发现重复时明确停止迁移，不静默删除或覆盖数据。
3. 重写事件归属解析为两步确定性逻辑：先按新 `turn_tasks.id` 精确查找；未命中时再按唯一 `legacy_task_id` 查找；两者都命中不同任务时返回“引用歧义”错误。不要继续使用无 `LIMIT` 的 `OR + query_row`。
4. 新提交在入口处校验 `legacy_task_id` 非空、格式合规，并让数据库唯一约束作为最后防线。

**必须增加的测试**

- 迁移遇到重复 `legacy_task_id` 时失败且版本不推进；
- 新 Task 与另一 Task 的 legacy ID 碰撞时提交失败；
- 事件按新 ID 命中、按 legacy ID 命中、ID/legacy ID 双命中冲突三种情况均有断言；
- 并发/重复提交不会产生第二个相同 legacy ID。

**验收条件**

任何合法事件引用最多只能解析到一个 TurnTask；重复 legacy ID 在数据库层和应用层都不可进入。

### R4 — 持久化安全边界与复合敏感字段仍不统一（1.1 → 1.2 仍未关闭）

**问题根因**

Task JSON 已统一经过 `safe_task_value`，但 Run/Session/Project 的标题、名称、路径、ID 等仍有直接写入路径；`authorizationHeader` 的普通值、`APIKey`、`tokenValue` 等字段名也可能绕过当前启发式匹配。仅测试 Bearer 文本不能证明持久化边界成立。

**具体实现方案**

1. 增加两个统一入口并禁止调用方直接写库：
   - `validate_persisted_id(field, value)`：限制长度和控制字符，拒绝换行/不可见字符、敏感标记和不符合 ID 格式的值；
   - `safe_metadata_text(field, value)`：限制长度和控制字符，先按统一敏感字段规则检查；对标题/名称/路径命中敏感规则时拒绝写入并返回字段名。
2. 将以下路径全部改用入口：旧 `create_run`、`create_run_with_idempotency`、`commit_turn` 中的 Run/Session 投影、`insert_project`、`insert_session`、`insert_agent`、历史 runs 查询前的兼容校验。
3. 敏感字段名先做大小写边界、短横线、点号和空格规范化，再按完整别名/后缀匹配，至少覆盖：`authorization`、`authorization_header`、`api_key`、`apikey`、`token`、`token_value`、`access_token`、`refresh_token`、`client_secret`、`provider_key`、`credential` 及 `_token/_key/_secret/_credential` 后缀。普通值也必须整值替换或拒绝，不能依赖是否包含 `Bearer`。
4. 对无法可靠识别的任意 JSON secret-bearing 字段，继续使用结构化安全 DTO；不允许新增未经 `safe_task_value`/`redact_persisted` 的 JSON 落盘入口。
5. 安全策略要明确：结构化凭据字段整值拒绝/脱敏；标题、ID、路径等索引字段优先拒绝，而不是把敏感内容改写后继续作为标识保存。

**必须增加的测试**

- Run title、Session title、Project name/root_path、Task/Run ID 含 token、API key、authorization 普通值时均拒绝；
- `authorizationHeader: "plain-secret"`、`APIKey: "plain-secret"`、`tokenValue: "plain-secret"`、嵌套数组/对象均不落明文；
- 旧 `create_run`、`commit_turn`、历史 runs 查询原始 SQLite 内容和 API 返回都通过断言；
- 正常标题、路径和 ID 的兼容回归测试，避免过度误杀。

**验收条件**

所有可达持久化入口都经过同一安全函数；新增一个旧/新创建路径的安全回归测试失败时，CI 必须阻止合并。

### 本轮新发现：同样必须在 A 放行前关闭

- **空 Turn**：`commit_turn` 允许 `tasks=[]`，但 opening event 需要合法 task。当前最小修复是在事务开始前明确拒绝空列表，并验证没有留下 Turn/Run/事件；若产品需要 Turn 级事件，再单独设计 `task_id=NULL` 的协议和查询，不得用 `turn.id` 冒充 Task。
- **未知依赖的查询语义**：`task_dependencies` 对不存在的 Task 返回空列表。先查询 `turn_tasks.id`，不存在时返回“执行任务不存在”，存在但无依赖时才返回空列表。
- **生命周期与组合创建**：状态机、Project/Session/Agent 原子创建仍是延期项。A 修复完成后 B/C 仍不得绕过这些接口直接暴露任意状态更新或独立创建 API。

## A 的最终返工顺序与放行门槛

1. 先完成 R1 幂等统一和 R2 全量 schema shape 检查，避免后续测试建立在错误数据库结构上。
2. 再完成 R3 legacy_task_id 唯一身份和事件解析重写。
3. 完成 R4 持久化安全边界与敏感字段测试。
4. 关闭空 Turn、未知依赖查询语义，补齐回归测试。
5. 运行 `build.ps1 -Action fmt/test/clippy/wasm-check`，并单独运行迁移、幂等、事件归属、安全边界测试。
6. 更新报告时逐项引用测试名称、迁移行为和公开 API 变化；不能只写“已修复”。

只有 R1-R4 和本轮新发现全部有代码、测试和门禁证据后，才重新评估是否放行员工 B；在此之前继续维持 `CHANGES_REQUIRED`。

## 连续延期项：不能再只写“后续处理”的落地路线

以下两组不是本轮新增，而是在前几轮持续被列为延期。它们可以拆给不同员工，但必须形成明确依赖和放行门槛。

### R5 — Project/Session/Agent 原子创建、输入约束和生命周期状态机

**现状**

多轮报告都承认 `insert_project`、`insert_session`、`insert_agent` 可被单独调用，`update_turn_status`/`update_task_status` 也没有强制状态转移图；ID、标题、路径和幂等 key 的长度、字符集、控制字符约束不统一。若直接由 B/C 暴露 HTTP，会产生孤儿对象、跨项目对象和非法状态跳转。

**分阶段方案**

1. **A 先冻结领域规则和 Repository 命令**：
   - 定义 `create_project_session_bundle(input)`，在一个事务内创建 Project、Session 和初始 Agents；保留底层 `insert_*_tx` 为私有函数；
   - 事务内验证 Project 存在性、Session.project_id、Agent.session_id，任何一步失败都回滚零行；
   - 定义 `validate_id`、`validate_title`、`validate_path`、`validate_idempotency_key` 的统一错误类型、长度和控制字符规则，并让所有 Repository 入口复用；
   - 定义 `LifecycleStatus::can_transition(from,to)`，在状态更新入口强制执行，并把状态事件写入同一事务。
2. **B 再接 Engine**：Engine 只能调用 bundle 和合法 transition 命令，不能直接拼 SQL 或调用公开的单表插入；恢复流程也只能走同一状态机。
3. **C 最后接 HTTP/SSE**：HTTP 创建接口只暴露 bundle；状态更新接口只接受目标状态，不接受任意字符串覆盖；错误统一映射为 400/404/409。

**必须测试**

- 中途 Agent 插入失败时 Project/Session/Agent 均为零行；
- 不存在 Project、跨 Session Agent、空/超长/含控制字符 ID 或标题、路径逃逸和重复幂等 key 均被拒绝；
- 合法状态链通过，非法跳转拒绝且不写状态事件；
- Engine 和 HTTP 不存在绕过 bundle/transition 的调用路径。

**放行条件**

A 交付领域命令和规则后，B 才能实现 Engine；C 必须等状态转换和组合创建测试通过后才能接入写接口。当前可继续做只读设计，但不能把现有分立插入函数当作产品 API。

### R6 — 核心事件命名空间与 Engine/HTTP 全链路接入

**现状**

`Event.kind` 仍是自由字符串，Engine 仍可走旧 `Store::create_run/event` 路径，Repository 的新 Project/Session/Turn/Task 链尚未成为唯一写入路径。只在 Repository 单元测试通过，不能证明取消、恢复、幂等和 SSE 游标在同一领域模型中成立。

**分阶段方案**

1. **A 定义协议边界**：冻结 protocol v1 的核心事件集合，例如 `turn.started`、`task.started`、`task.completed`、`turn.completed`、`turn.failed`、`turn.interrupted`、`approval.requested`；内部用枚举/受控构造器，保留 `unknown:<namespace>` 兼容未知扩展；禁止任意空 kind。
2. **B 接 Engine**：Engine start/continue/cancel/recover 统一调用 Project/Session/Turn/Task command；旧 `create_run` 只保留读取/兼容导入，不再作为新请求的写入入口；每次状态变化与事件、幂等记录同事务提交。
3. **C 接 HTTP/SSE/CLI**：HTTP 请求先做统一幂等 lookup，再创建/恢复 Turn；SSE 只读取 `events_after(session_id,cursor)`，cursor 必须单调且断线重连不重复；取消、恢复和审批都返回同一 Turn 状态模型。
4. **D 做跨链路审查**：验证明文不落盘、旧 key replay、重复请求、断线重连、取消后恢复、未知事件 kind 和错误映射。

**必须测试**

- Engine start → Store → events 的完整 Project/Session/Turn/Task 创建链；
- 同 key 同摘要 replay 不创建第二个 Turn，不同摘要返回 409 语义；
- cancel/recover 的状态、事件和 SSE cursor 同事务且可重放；
- HTTP/SSE 只使用稳定 command/query API；未知事件 kind 可读取但不能伪造核心状态。

**放行条件**

在 B/C 报告中逐项证明新链路已成为写入主路径，并给出旧路径只读兼容范围；在此之前，A 的 Repository 不能被描述为“后端流程已完成”。

## 交付格式要求

员工 A 下一轮报告必须按 `R1` 至 `R6` 逐项列出：修改文件/公开 API、失败场景、测试名称和门禁结果。若某项选择延期，必须写明接手员工、前置依赖、计划轮次和禁止调用的旧接口；只写“后续处理”视为未完成。

