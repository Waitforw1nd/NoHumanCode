# 员工 A Review — 第 1.5 轮

审查对象：niuma/员工A/提交报告/第一轮/员工A（Project-Session-Turn领域契约与SQLite Repository 第1.5轮报告）.md
审查日期：2026-09-22
审查人：项目总监
结论：CHANGES_REQUIRED

## 本轮实际门禁

已实际运行：

- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action fmt`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action clippy`
- `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action wasm-check`

结果通过：25 个库测试、4 个 adversarial 测试、9 个 runtime 测试、5 个 protocol 测试；live 测试仍忽略。门禁通过不代表 Repository 契约已经满足上一轮放行条件。

## 已确认完成

- `Store::commit_turn` 与 `Store::idempotency_replay` 已共用 `classify_both`，不再各自只查一张幂等表。
- 新建旧 Run 的标题已通过 `safe_metadata_text`。
- `replace_dependencies` 已先校验全部依赖，再删除旧关系；`Repository::commit_turn` 已收紧为 `pub(crate)`。
- 报告诚实标明 R2、R3、R4、R5、R6 尚未全部完成。

## 必须修复

### P1 — R1 只收口 Store，Engine/旧写路径仍绕过统一 replay

`classify_both` 已被 Store 内部两个方法复用，但 `NoManCode/rust-app/src/engine.rs` 仍在 `start_idempotent` 中调用 `store.idempotent_run`，并继续使用 `create_run_with_idempotency`。HTTP `/start` 进入 Engine 后仍走旧 `idempotency` 表语义，实际产品写路径没有使用 `idempotency_replay` 的显式 `Turn/LegacyRun` 结果。

**具体修复**

1. 新请求的 Engine 入口必须先调用统一 replay/classifier；旧 Run 命中时明确进入兼容分支或返回迁移错误，不能继续当作“没有 key”。
2. 旧 `idempotent_run`/`create_run_with_idempotency` 只能保留为只读兼容导入，禁止成为新 HTTP 请求的写入口。
3. 全仓搜索旧 API 调用，报告中列出每个剩余调用及其只读用途。
4. 增加 Engine → Store 的双表同 key、旧 key、冲突 key 和重复请求测试，确认不新增 Run/Turn。

### P1 — R1 的 LegacyRun replay 丢失旧任务内容

`classify_both` 为旧表命中构造 `IdempotencyReplay::LegacyRun(Run)` 时把 `tasks` 固定为 `vec![]`。旧 `self.run(&run_id)` 路径原本会读取旧 Run 的任务；现在兼容 replay 返回的 Run 不再等价于原对象，调用方可能丢失任务列表或错误判断旧运行为空。

**具体修复**

- 统一从旧 `runs/tasks` 读取完整 Run，再包装为 `LegacyRun`；
- 对任务 JSON 解析失败、旧 Run 不存在、旧任务归属错误分别返回数据损坏错误；
- 增加旧 Run 含多个 Task 的 replay 测试，断言 ID、顺序、数量和安全脱敏结果保持一致。

### P1 — R2 schema shape 仍明确未完成

`verify_or_reject_partial_tables` 仍只检查七张表的列名和 `legacy_task_id` 索引名称，没有验证：

- 主键与复合主键顺序；
- 外键目标和列；
- `PRAGMA index_list` 的 `unique=1`；
- `PRAGMA index_info` 的列顺序；
- NOT NULL 和关键索引。

因此错误 PK、缺 FK、普通同名索引仍可能通过迁移并推进 schema 6。

**具体修复**

建立统一 SchemaShape 元数据，并在迁移事务中使用 `table_info`、`foreign_key_list`、`index_list`、`index_info` 全量核对七张表。每张表构造缺列、错误 PK、缺 FK、普通索引冒充 UNIQUE 的启动失败测试；失败时必须保持 migration marker 和 `user_version` 不变。

### P1 — R3 仍按 TurnId 判断歧义，历史交叉碰撞未迁移检测

报告已经承认 `resolve_task_turn` 比较的仍是 `turn_id`。两个不同 Task 属于同一 Turn 时，按 ID 和 legacy ID 同时命中会得到相同 TurnId，事件仍会被接受，无法确定具体 Task。迁移也没有检测已有数据中 `task.id == other.legacy_task_id` 的交叉碰撞。

**具体修复**

- 解析结果同时携带 TaskId 与 TurnId；两个不同 Task 命中即返回歧义，无论是否同一 Turn；
- schema 迁移增加历史交叉碰撞查询，发现后停止迁移并报告行身份；
- 补同 Turn、跨 Turn、已有历史碰撞、正常 legacy fallback 四类测试。

### P1 — R4 的公开写入口和输入约束仍未收口

以下问题仍存在：

- `Repository::insert_project`、`insert_session`、`insert_agent` 仍公开裸写；
- `Store::insert_agent` 没有对 Agent ID、Session ID、display_name、role 全部使用统一安全函数；
- `insert_turn_tx`/`insert_turn_task_tx` 没有统一验证 Turn、Session、Project、Agent、Task、legacy_task_id；空字符串、控制字符和超长标识仍可入库；
- 幂等 key、request hash 仍未统一执行 ID/文本约束；
- `safe_metadata_text` 对嵌入在字符串中间的 token 仍可能漏检；
- 已有 Run 被复用时没有再次验证 title/kind 是否安全一致。

**具体修复**

1. 将 `validate_persisted_id`、`safe_metadata_text` 设为所有 Store/Repository 可达入口的唯一边界；底层事务写函数改为私有或 `pub(crate)`。
2. 所有 ID/key/title/Agent 文本统一限制长度、控制字符、空值和敏感内容；`legacy_task_id` 还必须拒绝空字符串。
3. 元数据扫描所有位置，命中 token、authorization、api key、secret、credential、password 即拒绝；不确定时拒绝写入。
4. 旧 Run 已存在时也要验证兼容投影；不安全历史数据只能显式迁移或只读返回，不能被新提交无条件复用。
5. 增加 Agent、Turn/Task ID、幂等 key、嵌入式 `hello sk-token`、正常中文标题/路径和原始 SQLite/API 回读测试。

### P1 — `replace_dependencies` 虽已调整顺序，仍应收紧 API 和环检查边界

当前先校验再删除已解决主要清空风险，但 `replace_dependencies` 仍是公开方法，调用方可以绕过完整 Turn 提交流程单独改关系；环检测在 `commit_turn` 后执行，单独调用仍可能写入形成环。

**具体修复**

- 将依赖替换收进不可绕过的 Turn command；
- 在同一事务内完成存在性、同 Turn、自依赖、环检测后再写入；
- 为“已有依赖 + 非法依赖”“单独替换形成环”“外层错误后提交”增加数据保持测试。

## R5/R6 仍是发布阻塞项

R5 继续延期：组合创建、统一输入约束和状态转移图尚无代码，B/C 不得暴露公开单表插入和任意状态更新。

R6 继续延期：Engine 仍使用旧 `create_run`/旧幂等路径，核心事件 kind 仍为自由字符串，新 Project/Session/Turn/Task 链路尚未成为唯一写入路径。B/C 不能开始产品写接口，只能做只读设计和适配准备。

## 下一轮放行门槛

1. Engine 新写请求迁移到统一 replay/Turn command，并保留完整 LegacyRun replay；
2. 完成七张表真实 PK/FK/UNIQUE/index shape 校验；
3. 按 Task 行身份拒绝事件歧义，并拒绝历史交叉碰撞；
4. 收口 Repository 裸写、所有标识/文本输入和已有 Run 投影；
5. 依赖关系只能通过完整事务 command 修改；
6. R5/R6 未完成前，不冻结 Repository，不向员工 B/C 发放实现许可。

当前维持 `CHANGES_REQUIRED`。
