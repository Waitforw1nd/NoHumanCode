# NEXT-21：持久绑定与恢复独立设计审查

2026-11-01 · 员工 C · 第二十二轮 · 只读设计意见，待经理裁决；不授权实现。

## 按严重度排列的缺口与可复现反例

1. **P0：撤销和迟到决策交叉。** Worker 发出合法响应前，撤销事务先关闭准入；若 Host 仅在启动时检查，仍可能采用该响应。更严重的是，已经由 Host claim 的 Shell、网络或文件效果可能正在发生，拒绝该决策不等于撤销这些效果。必须在响应采纳事务内比较请求 ID、绑定代和撤销门，且继续使用现有审批、工具结果及 unknown 事实，禁止假装回滚或自动重放。
2. **P0：崩溃后把未知当作已退出。** Host 在启动 worker 后死亡，重启看到 Task interrupted 就释放租约，仍存活的旧 worker 可能与新执行重叠。`store.rs:2256` 的恢复将 queued/running 标记 interrupted，并将已 claim 的审批等标为 unknown；它并没有 worker 退出证据。超时、owner 死亡、PID 和创建时间均不能单独证明隔离域已空；PID 可复用。未经证明的 Job 行为不能成为通行证。
3. **P1：绑定与持久化不原子。** Team 的部分 Task 已插入，随后发现外部策略 disabled 或修订过期；若补偿失败就留下一组身份不一致的任务。`commit_turn_bundle_inner` 的 IMMEDIATE 事务已覆盖 Turn、任务、回执及现有绑定（`store.rs:1119`）；新绑定必须进入同一事务并覆盖每个成员。`append_chat_turn_inner` 类似（`:1386`），不能事务外补写。
4. **P1：resume 拒绝太晚或换身份。** 旧任务已绑定 A，项目改选 B；resume 若按当前选择调用 B，历史变义；若先提交消息、resume 事件再发现 A 已撤销，则留下半完成记录。`resume_task` 在 IMMEDIATE 事务里先校验，再 `save_task_tx` 和写 delta/resume（`:869`），新绑定校验必须放在写入之前且同一事务。
5. **P1：旧入口绕过或重放被改写。** `Engine::start` / `start_idempotent` 最终读取当前项目（`engine.rs:1475`）；Store 公开的 `create_run`、`create_run_with_idempotency`、`commit_turn`、`commit_turn_bundle` 和未授权版 `append_chat_turn` 不能因为没有新参数而默认 builtin 绕过外部选择。同 key 同请求在选择变更或 disable 后仍须返回原回执；不同请求冲突，不能重新执行。`commit_turn_bundle_inner` 和 append 已在事务开头分类重放；这一顺序不可倒置。
6. **P1：把单 worker 与 Task guard 混同。** `run_task` 在依赖等待之前获取 builtin Agent lease（`engine.rs:1908`），后续在 `:1993`、`:2106` 两次决策。第一次 worker 退出不表示 Task 结束。`agent_runtime.rs:325` 的 `in_flight` 由当前 runtime 的内存维护；`main.rs:94` 锁住正常 Host 的数据目录，`--check` 例外，独立 `Store::open` 也不自动获得该锁，不能声称计数跨 Store/进程全局。
7. **P2：旧闭集被动态外部实体污染。** CLI 对 composition 与 Agent runtime 做字段/状态验证（`cli.rs:391,2001`），服务现有插件及组件路由在 `server.rs:409` 附近。把外部插件混入旧列表、改 builtin 的默认含义或增加未约定字段，可能破坏旧客户端并误报激活状态。

## 推荐冻结的最小契约

### 身份、选择、生命周期

- 只允许纯 `agent.strategy@1.0.0` 的受限决策协议。版本、内容摘要、授权是不同事实：工件以插件标识、接口版本、内容摘要唯一确定，授权独立可撤销；不传消息、任意路径、参数、密钥或 DB 句柄，也不允许 worker 直接实施 Host 效果。
- 每项目选择是一条版本化指针：builtin 或已登记且当前获准的外部工件；更新使用 expected selection revision CAS。新 Turn 固定事务内读取的选择，Team 所有 Task 均生成不可变绑定 `(task_id, kind, plugin_id, interface_version, digest, selection_revision, desired_revision, binding_generation)`；builtin 也在新任务明确记录。原 Task resume 只读原绑定，不随当前选择迁移。选择切换或 upgrade 的提交只影响之后新建 Task；不改变已有 Task。
- 旧 Task 无绑定行解释为**历史 builtin**，不是读取当前选择。迁移以事务和 schema 版本建立约束；新路径必须写绑定，不能通过删行变成历史 builtin。推荐记录迁移边界或显式补齐历史 builtin 并保证新写入不能缺行；对无法判明历史与新记录的损坏 fail closed。避免把旧身份臆造为外部身份。
- disable 是停止新 Task/新 resume 的准入；已持 Task guard 的工作按既有 draining 约定运行，除非明确调用 revoke。upgrade 产生新摘要/代，不原地替换旧身份；旧工件不可用时旧任务 fail closed。desired revision 记录绑定时的事实，运行校验必须明确允许的当前授权及 gate；不能只因之后无关选择 revision 变化就把旧绑定改成新身份。revoke 更强：事务关闭该绑定代/授权域的后续启动及结果采纳，发出取消与清理请求。
- Task 级 master builtin guard 与外部 guard 从运行准入持有至整个 Task 执行退出，包含依赖/Key 等待、所有决策与清理；单次 `decide` 的 worker 请求独立产生、回收。guard 是当前 Host 的进程内生命周期事实，不等于持久 worker 证明，也不得重定义旧 `in_flight/draining` 为全局值。跨 Store 的准入与 disable/revoke 在线性化事务内检查共享持久门；不能依赖进程 mutex 或事前读。锁顺序须固定，任何耗时启动、取消和等待均在数据库事务及 runtime mutex 外。

### 最小持久事实（逻辑模型，不强制六张表）

1. 工件登记与授权：不可变 `(plugin_id, interface_version, digest)` 及验证状态；独立 desired enabled/revision、授权代和撤销门。工件内容用受控内部引用，登记时及使用前核验摘要；禁止 API 指定任意可执行路径。是否合表取决于已有可复用事实，不复用只接受 builtin 闭集 ID 的旧管理表以冒充外部支持。
2. `project_strategy_selection(project_id PK, kind, identity?, revision)` 和 `task_strategy_binding(task_id PK/FK, immutable identity, selection_revision, desired_revision, generation)`；加 kind 与可空字段一致性、唯一性、外键、非负 revision 及不可变约束，删改策略明确 fail closed。项目不存在选择时默认 builtin；旧任务迁移语义如上。
3. 外部决策的持久执行记录：唯一 request ID、task/binding generation、owner epoch、阶段和受控隔离域身份/证据引用、结果采纳状态。启动前先持久登记意图，采用能证明从创建到纳入隔离域没有逃逸窗口的启动协议；若在意图与启动之间崩溃且无法证明未启动，同样归 unknown。不能靠 Task 状态或 RAM guard 推断已无 worker。具体记录可与操作表合并，但必须能追踪多个请求、区分代并防 ABA。
4. 撤销操作记录和幂等回执：唯一作用域+key+请求摘要，记录 expected revision、操作 ID、requested 与最终状态/证据版本；状态至少 `requested`, `completed`, `cleanup_unknown`。历史回执不可被后续状态改写；状态查询单独返回当前进展。可借鉴 `plugin_management.rs:173-212,217-285` 的先重放后 CAS 与同事务回执，但其 builtin ID 闭集和 key 语义不能不经设计直接扩展。

任务本身已有状态、审批与恢复事实，单实例正常启动也已有目录锁；不另建可从这些事实可靠推导的全局 Task 计数表。worker 记录不是普通进程内计数：其存在是为了崩溃后辨别潜在存活，能否缩减须以 A 的证明及恢复实验为前提。

### 事务与入口

```text
new_turn / initial_team (IMMEDIATE):
  classify(key, request_hash); replay => return historical receipt; conflict => reject
  validate project/turn/predecessor and current project selection/revision
  validate master desired + external authorization/desired/revoke gate
  insert turn, all legacy/domain tasks, idempotency receipt
  insert immutable binding for EVERY task from same selected snapshot
  commit; on any error rollback all

resume(task) (IMMEDIATE):
  read immutable binding (legacy missing => builtin only under migration rule)
  validate original identity, master/external gates, artifact, unknown and existing resume checks
  only then save task/message and resume events; commit

admit/task or worker-result (IMMEDIATE):
  verify task binding generation, relevant gate and request identity
  for worker result additionally CAS pending -> accepted, persist sanitized decision once
  commit; revoked/stale/duplicate/unknown => reject without effects

revoke (IMMEDIATE):
  classify operation key/hash first; replay historical receipt or reject conflict
  CAS expected authorization/gate revision; close gate, advance generation
  insert requested receipt + mark affected worker records for cleanup; commit
outside locks: cancel, terminate and wait via proven isolation mechanism
follow-up (IMMEDIATE): CAS same operation/generation/worker evidence;
  completed only after all applicable worker exits are proven, else cleanup_unknown
```

采纳事务必须与撤销 gate 更新使用同一 SQLite 写序列：先提交采纳的决策属于已采纳事实；先提交撤销则丢弃迟到响应。若采纳之后、效果 claim 之前撤销，效果 claim 也须同事务复核 gate；claim 已先提交则由现有执行/审批和 unknown 恢复规则处理，撤销状态不能承诺效果已回滚。调用方重试只返回原 receipt，不重新执行。

`start` / `start_idempotent` / `start_in_project` 都汇入 `start_project_locked`，需将共同绑定函数透传到 `commit_turn_bundle_inner`；`send_chat_turn` 经 `append_chat_turn_inner` 对新 Task 使用当前选择，而非继承旧 Task 的策略。`Store::commit_turn`、`commit_turn_bundle`、`append_chat_turn` 的公开低层变体必须共同进入绑定事务，或在能确定关联项目存在外部选择时事务内明确拒绝；不可先在外层读完再写。`create_run` 和 `create_run_with_idempotency` 若无法可靠解析项目选择，应直接拒绝新建或只在可证明的历史专用场景允许，不能按 builtin 猜测；已有历史读取与同 key 重放保留。所有入口包括直接 Store 调用都需要覆盖，不能仅依赖 HTTP/Engine。resume 的 Engine 预检可提早报错，但以 `resume_task` 同事务、写消息之前的复核为权威。

### 崩溃、核验和解除

- `requested` 只证明持久门已关及请求已受理，不证明 worker 已结束；锁外取消、终止并等待。可验证的隔离域已空、所有对应请求无法再产生被采纳的响应且清理事实持久提交，才能标 completed。正常退出时的可信句柄等待结果也必须绑定同一 request/代。Host 死亡后不存在证明时一律 `cleanup_unknown`，不自动重启原绑定。
- 提供受认证的 `verify_cleanup(operation_id, expected_state_version, evidence_ref, idempotency_key)`：先按 key/摘要重放或冲突；可信 Host/supervisor 读取受控证据源，核验隔离域和请求/代一致、证据真实性及新鲜度，必要时主动终止并等待；随后 IMMEDIATE 事务 CAS 仍是同一 unknown 操作、相同 gate 和证据代，再写审计回执与 completed。客户端不能提交任意声称已退出的布尔值或路径；超时、PID、owner 死亡均不构成证明。重复请求返回相同回执，过期证据/版本冲突失败关闭。
- 可行证据候选是仍持原隔离域可信句柄的独立 supervisor 对对应域清空的可认证见证，或经 A 证明跨 Host 崩溃仍可靠的操作系统机制与可验证退出证据。进程创建原子纳入 Job、kill-on-last-handle、句柄不可继承和退出/进程树证据都是**待 A 实测的依赖，不是已成立事实**。若无可验证证据，操作保持 unknown；经认证的管理员可先关闭该代全部准入、停机并通过可信系统管理手段清理/隔离，只有产品能验证绑定到该隔离域的可信清理证明后才能提交同一 CAS 解除。若系统无法产生这样的证明，该代不能解除；可另建新代的前提是经验证的隔离能阻断旧代访问及共享副作用，且旧代继续隔离审计。若连隔离独立性都无法证明，则保持阻塞并要求修复隔离/重建受信环境，绝不提供万能手动解封。
- 恢复后保留旧审批、文件/网络/Shell 的 claimed/unknown；独立核验外部 worker 清理不能将它们变为成功、失败或重放。恢复必须先封门并识别未决 worker，再允许新外部启动。

### API 与兼容

`GET /api/host/composition`、`/api/plugins`、`/api/runtime/components` v1 的原有闭集、字段、builtin 默认意义及本实例观测维持不变。新增独立、认证且同源约束的操作：项目策略选择 GET/PUT（kind、插件 ID、接口版本、digest、selection revision；PUT 带 expected revision 和幂等 key）；Task binding/status GET（固定身份、binding generation、准入和清理状态）；revoke POST 及 operation GET/verify（作用域、expected revision/state version、key、操作 ID、脱敏状态与可验证证据引用）。响应只返回规范身份与固定错误码，不返回 worker 原始输出、任意文件路径、参数或凭据。状态与历史回执分开，版本、摘要、授权分别展示。失败、冲突、未知和不支持均应明确可区分。

### 对 B 草案的裁决建议

保留纯策略接口、不可变摘要绑定、master/external 双域与每次决策单独 worker；保留 requested 与清理终态分离。收敛为以上最少持久不变量，暂不预设六组表全部必要；保留 worker 持久身份与未知状态，因为仅有 Task 和内存计数无法排除宿主崩溃后的存活。对 LPAC、ACL、Job、资源上限和句柄语义只列 A 的待证明条件，不视为已经通过。不可用的证明路径不得包装成已完成恢复能力。

## 验证映射及本轮证据

| 验证场景 | 必须观察的结果 |
| --- | --- |
| Team 多任务注入绑定失败、选择/disable 与创建交错 | 全部原子回滚；成功任务都绑定同一事务选择；跨两个 Store 按提交顺序裁决 |
| 同 key 重放后选择切换/disable，异 hash | 原回执原身份不变；异 hash 冲突；无新执行 |
| 后续 Turn 与旧 Task resume | 新 Turn 绑定当前选择，resume 使用原身份；stale/disabled/unknown 时消息、事件、任务均无新写入 |
| 各 Engine、Store 兼容入口及历史无行 | 外部选择不可绕过；历史 builtin 不迁移；无法判定的缺行 fail closed |
| master/external disable、两次 decide、依赖等待 | Task guard 全程有效；单 worker 退出不释放 Task guard；旧状态只反映本实例 |
| revoke 与响应采纳/效果 claim 竞争 | 以事务提交顺序裁决；迟到结果丢弃；已 claim 效果按原规则核验 unknown，不重放 |
| 注入启动前后和终止等待中真实 Host kill、PID 复用、supervisor 丢失 | 没有同代可信退出证明时 unknown；伪证、旧代证据、旧 CAS 均拒绝；具备可信证据时认证核验、重复幂等并可完成 |
| A 隔离/句柄前提及 API 兼容 | 首先以独立实验证明启动无逃逸和可恢复证明；旧三个 v1 闭集及 CLI 保持原样，新 DTO 无原始输出/路径/凭据 |

审查基线：指定 `17dae540c245fd415a5537df20ff925985abb478` 为当时 HEAD `94a9e4ef1024931656bbbd6513bf6d2d1b19123e` 祖先；产品树 `897cc8510c0c56118df04c2c1dbcea9c999e539f`、Rust 树 `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`，基线到 HEAD 产品 diff 和检查时工作区产品 diff 均为空；index/unmerged 空。继承的人员文件改动保留。本轮只读源码与设计，**未构建、未测试、未运行 Host 或隔离探针**；动态结论未验证。A 实验尚未提供本报告所需的隔离与恢复证据，经理须结合 A 结果裁决后才能冻结下一实现任务。
