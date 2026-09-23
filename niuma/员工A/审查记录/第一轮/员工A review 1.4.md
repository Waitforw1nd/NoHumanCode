# 员工 A Review — 第 1.4 轮

审查对象：niuma/员工A/提交报告/第一轮/员工A（Project-Session-Turn领域契约与SQLite Repository 第1.4轮报告）.md
审查日期：2026-09-22
审查人：项目总监
结论：CHANGES_REQUIRED

## 本轮实际门禁

已实际运行：

- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action wasm-check`

结果通过：25 个库测试、4 个 adversarial 测试、9 个 runtime 测试、5 个 protocol 测试；live 测试仍忽略。门禁通过，但 R2 和 R4 在报告中仍明确未完成，且 R1/R3 还有实际写入路径未统一。

## 已确认完成

- `resolve_task_turn` 已取代事件归属的 `id OR legacy_task_id` 查询，并加入新 ID 与已有 legacy ID 的提交期碰撞检查。
- `idempotency_replay` 会同时读取新旧幂等表，旧 Run 不再伪造为 Turn。
- `APIKey`、`tokenValue`、`databasePassword` 已使用不同测试值，密码后缀规则已恢复。
- 空 Turn 和未知 Task 依赖查询已拒绝。
- 报告没有把 R2、R5、R6 误报为完成。

## 必须修复

### P1 — `commit_turn` 仍未使用双表幂等一致性判定

`Store::idempotency_replay` 已经同时读取两张表，但 `Store::commit_turn` 仍走旧的 `classify_idempotency` 分支：新表命中 `Same` 后直接返回原 Turn，不再查询旧 `idempotency`。因此“新表同 key、旧表同 key 但摘要/Run 不一致”的损坏状态，在实际写入入口仍会被静默接受。

**具体修复**

1. 抽出同一事务内的 `classify_idempotency_both`，同时返回新表和旧表命中结果。
2. `idempotency_replay` 和 `commit_turn` 共用该函数，禁止一条路径只查新表。
3. 任一侧摘要冲突返回 Conflict；两侧同摘要时校验新 Turn 的 Session.legacy_run_id 与旧 run_id 一致；无法证明一致时返回数据损坏错误。
4. 增加双表同 key 同摘要、不同摘要、新 Turn/旧 Run 不同绑定三组测试，并确认不新增 Run/Turn。

### P1 — R2 schema shape 仍未关闭

报告明确承认仍只检查必需列和唯一索引名称。`verify_or_reject_partial_tables` 没有验证主键、复合主键、外键目标、NOT NULL 或索引的 `unique=1` 与列顺序；一个同名普通索引或错误外键结构仍可能推进到 schema 6。

**具体修复**

- 用统一 SchemaShape 元数据覆盖七张新表；
- 用 `PRAGMA table_info` 检查列、NOT NULL、PK 顺序；
- 用 `PRAGMA foreign_key_list` 检查每个引用目标；
- 用 `PRAGMA index_list/index_info` 检查唯一索引和关键索引的属性及列顺序；
- 检查 `turn_task_dependencies(task_id, depends_on_task_id)` 复合主键和 `idempotency_records.key` 主键；
- 缺结构时在打开数据库阶段失败，且不推进 marker/user_version；
- 每张表增加缺列、错误 PK、缺 FK、普通索引冒充 UNIQUE 的迁移测试。

### P1 — R3 仍不能识别同一回合内的任务歧义和历史交叉碰撞

`resolve_task_turn` 只比较 `by_id` 与 `by_legacy` 返回的 `turn_id`。如果两个不同 Task 恰好属于同一 Turn，两个 TurnId 相同，函数会接受事件，仍不能确定事件属于哪个 Task。迁移检查也只查重复 `legacy_task_id`，没有检查已有数据中 `task.id == other.legacy_task_id` 的交叉碰撞。

**具体修复**

1. 解析函数返回 Task 身份（或同时返回 task_id 与 turn_id），比较的是行身份，不只是 turn_id；任何两个不同 Task 命中都返回歧义错误。
2. schema 迁移增加全局交叉碰撞查询；发现历史碰撞时停止迁移并报告两行身份。
3. 提交期继续保留新 ID/legacy ID 交叉检查，并覆盖同 Turn 和跨 Turn 两种情况。
4. 增加同 Turn 双命中、跨 Turn 双命中、历史数据迁移失败测试。

### P1 — R4 的安全边界仍有明确未收口路径

报告已承认以下问题仍存在，不能放行：

- `Repository::insert_project`、`insert_session`、`insert_agent` 仍是公开裸写入口；
- `Store::insert_agent` 未把 Agent ID/session ID、display_name、role 全部送入统一安全函数；
- `commit_turn` 创建旧 Run 时仍直接把 `session.title` 写入 `runs`，未调用 `insert_legacy_run`/`safe_metadata_text`；
- Turn/Task/Agent/Project/Session/legacy ID、幂等 key、request hash 没有全部统一调用 `validate_persisted_id`；
- `safe_metadata_text` 对字符串中间出现的 token 仍不能完整拒绝，`token_end(value, 0)` 只检查开头。

**具体修复**

1. 把 `PersistedId`、`safe_metadata_text` 作为所有 Store 和 Repository 可达入口的唯一边界；底层 `*_tx` 改为私有或 `pub(crate)`。
2. `commit_turn` 的旧 Run 投影调用安全插入函数；Agent 和所有 TurnTask 标识在插入事务最前面统一校验。
3. 幂等 key/request hash 采用长度、控制字符和敏感内容校验；历史 API/SQLite 回读也加断言。
4. 元数据文本扫描所有位置，命中 token、authorization、api key、secret、credential、password 即拒绝；不确定时拒绝写入。
5. 测试必须分别覆盖 Agent、commit_turn Run title、嵌入式 `hello sk-token`、普通 authorization/APIKey/password 值，且使用互不相同的值。

### P1 — R1/R3 的测试仍不足以证明报告结论

`legacy_idempotency_replay_is_explicit` 主要覆盖旧表单独命中和新 ID/legacy ID 的一个提交失败；没有覆盖上述双表损坏组合，也没有覆盖“两个不同 Task 在同一 Turn 命中同一事件引用”。测试名称通过不等于契约已闭环，下一轮报告必须逐项列出失败前后的数据库行数、错误类型和回读结果。

## R5/R6 仍是发布阻塞项

R5 继续延期是诚实的，但四轮都未落地：A 必须先交付 Project/Session/Agent 组合创建、统一输入校验和合法状态转移；B 才能接 Engine；C 才能接 HTTP 写接口。

R6 也继续延期：Engine 仍可能使用旧 `create_run`，核心事件 kind 仍为字符串。A 先冻结事件集合和兼容 unknown 策略，B 再迁移 Engine 写路径，C 接 HTTP/SSE，D 做跨链路验收。B/C 不得把公开单表插入、任意状态更新或旧 `create_run` 作为新产品写入 API。

## 下一轮放行门槛

1. `commit_turn` 和 replay API 共用双表幂等分类；
2. 完成七张表的真实 PK/FK/UNIQUE/index shape 校验；
3. 事件解析按 Task 行身份拒绝同 Turn 歧义，并在迁移阶段拒绝历史交叉碰撞；
4. 收口所有 Agent/Run/Turn/Task/幂等字段和公开 Repository 写入口；
5. 补齐上述失败测试后重新运行 fmt、test、clippy、wasm-check；
6. R5/R6 未完成前，不冻结 Repository，也不向员工 B/C 发放实现放行。

当前维持 `CHANGES_REQUIRED`。

## 补充复核：Repository 层不变量仍可被绕过

### P1 — 空 Turn 只在 Store 检查，底层 Repository 仍可直接提交

`Store::commit_turn` 已拒绝 `tasks=[]`，但 `Repository::commit_turn` 仍是公开函数，没有同样的不变量检查；直接调用底层函数会尝试写入没有合法 Task 的 opening event。领域约束不能只放在一个包装层，否则 B/C 或未来内部调用会绕过它。

**修复要求**

- 将 `Repository::commit_turn` 改为 `pub(crate)`/私有事务实现，统一由 Store command 调用；或在 Repository 层同样拒绝空任务并验证 opening event 的 Task 归属；
- 为直接 Repository 调用增加空 Turn、错误 opening event 和回滚测试。

### P1 — `replace_dependencies` 先删除再校验，公开调用可能清空原依赖

`Repository::replace_dependencies` 先执行 `DELETE`，随后才逐个查询依赖任务和写入。如果调用者捕获错误后继续提交外层事务，原依赖已被删除，形成静默数据损坏。

**修复要求**

- 先在内存/临时集合中完成全部存在性、同 Turn、自依赖和环检查，再执行 DELETE + INSERT；
- 或把函数改为私有并强制在不可吞错的事务命令中调用；
- 增加“已有依赖 + 新依赖非法 + 外层提交”的测试，断言旧依赖仍完整。

### P1 — 已存在的旧 Run 投影没有再次验证

`Store::commit_turn` 只有在 Run 不存在时才写标题；Run 已存在时不检查其 title/kind 是否与当前 Session 一致，也不检查旧数据是否已经含敏感内容。兼容读取/迁移路径必须明确是只读、拒绝不安全历史数据，还是先清洗后再继续，不能在新提交时无条件复用。

## 结论保持

上述问题与 R1/R2/R4、R5/R6 一起构成放行阻塞项。当前继续 `CHANGES_REQUIRED`，不冻结 Repository，不向 B/C 发放实现许可。
