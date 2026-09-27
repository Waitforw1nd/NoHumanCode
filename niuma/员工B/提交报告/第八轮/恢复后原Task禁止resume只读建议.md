# 恢复后原Task禁止resume只读建议

员工B/GPT-6-astra，2026-09-27；同B-R8支持任务。仅只读源码，未改产品、未跑测试，等待经理补充契约。当前建议不倒推NEXT-06已承诺strict resume拒绝，也不改变860历史根因未明事实。

## 现状与最小事务边界

Store::resume_task在BEGIN IMMEDIATE中先检查checkpoints(task_id)，存在即CheckpointError::Conflict；随后项目校验、最新Turn检查、save_task_tx和delta/resume事件，最后commit。workspace_restores目前没有在该入口检查。Engine旧测试明确允许complete/partial/unknown恢复后的resume启动，再在工具写入层拒绝。这证明“永久写封存”与“任何resume禁止”是两项不同契约。

最小严格化是同一个Immediate事务内，在任何save_task/event之前检查`EXISTS(checkpoints...) OR EXISTS(workspace_restores...)`，后者只按task_id查存在，不筛status。于是Task restore与Checkpoint restore两条路由产生的complete/partial/unknown/claimed事实均拒绝，包含当前仅创建checkpoint尚未恢复的原有拒绝语义。无需迁移、删除历史行或改Task JSON。

注意零文件/不可恢复/前置冲突若从未产生workspace_restores事实，不能仅因用户曾调用restore就禁用resume；若产品要连这类尝试也封存，需要另立持久事实，不在最小范围。两条“路由”指两条恢复路由；当前resume HTTP只有/api/tasks/{id}/resume，CLI也调用它。

Immediate锁使resume事务与claim_restore事务有明确先后：restore claim先提交则resume拒绝；resume先提交仍允许该次resume，不能取消已合法完成的事实。若经理要求跨Engine运行中的恢复也全局排他，另属更大运行状态契约，不应靠本次EXISTS假称已经解决。

## 还需修改Engine的一处前置内存副作用

Engine::resume目前在store.resume_task调用之前，waiting_for_approval分支会向deferred_resume插消息。若新增Store封存拒绝，数据库虽无变化却可能留内存项。建议将该insert移至Store事务成功之后、launch之前；task本地clone的消息/status变更本身未落库，不构成外部副作用。不要只加Engine早期latest_restore查询：该查询不能替代Store事务检查，会有TOCTOU。

Engine此前审批unknown、network preflight、scope冲突可以先返回其他有效拒绝。契约应明确“所有恢复事实均不能resume”与“sealed错误在所有其他错误之前”并非同一要求。推荐只对可达Store的封存分支固定错误，对其他有效安全拒绝不扩大重排；若要HTTP所有封存Task一律sealed，需新增只读早查用于错误优先级且最终事务检查仍保留。

## 稳定错误方案

当前server::resume直接`?`进入ApiError通用classify_anyhow，只有IdempotencyConflict映射409，其他普通anyhow默认400/request_failed。故仅新增Store错误不能保证C期望的409。

推荐：在store.rs定义小型typed `ResumeError::Sealed`（名称由经理冻结），固定Display“任务已封存，不能继续原任务”；C在resume handler针对该类型显式映射HTTP409、现有protocol ErrorCode::Conflict、retryable=false，固定脱敏消息。不新增协议枚举/DTO/schema；CLI可沿用现有结构化HTTP错误消费。checkpoint封存与restore封存可统一该类型，需同步原checkpoint错误断言。

更小的备选是复用WorkspaceChangeError::Conflict { receipt: None }，resume handler显式经workspace_change_error映射409；缺点是内部无法清晰区分封存与一般工作区冲突。继续复用CheckpointError::Conflict扩展所有restore最省行数，但无checkpoint的历史会显示“检查点状态冲突”，不推荐作为长期语义。错误不得含SQL、OS码或原始文件内容。

## 所有权补充建议

- B需由经理明确扩权src/store.rs（当前NEXT-09只有engine/lib），新增事务guard/typed错误及Store原子性测试；engine.rs更新deferred_resume顺序和内嵌旧测试已属本人。
- C需明确扩权src/server.rs及必要HTTP映射测试（当前只restore_http_tests/workspace_http），固定resume错误映射；CLI如无需代码改动不扩大文件所有权。src/restore_http_tests.rs已有strict R04继续由C。
- D继续只读审事务/锁顺序/兼容变化，不能代改。
- 不必修改workspace_changes/checkpoint实现、schema、Cargo、provider、网络授权、project sidecar。若采用新增协议code则会扩大protocol/CLI契约，本建议避开该扩张。

## 已定位测试影响与必要正控

1. engine::workspace_phase_tests::restore_gate_serializes_competitors_and_seals_resumed_writes：complete恢复后resume.unwrap及新审批执行链，应改为typed拒绝，并直接比较Task JSON、事件数量、审批/changes/restore receipt、磁盘和模型调用数无新增；保留前半竞争/单receipt证据。
2. engine::workspace_phase_tests::partial_and_unknown_restore_histories_seal_resumed_writes：两状态resume.unwrap应同样改为入口拒绝和零副作用；不删除partial/unknown原receipt与写封存断言。
3. checkpoint测试已有resume拒绝正向，需若改类型则更新精确错误断言；不能只保留is_err掩盖任意错误。
4. tests/workspace_changes.rs::another_task_edit_blocks_resumed_write_without_absorbing_external_bytes没有restore事实，其resume成功必须保持，避免把“存在workspace_changes”错误当封存条件。
5. 现有runtime/session/final_acceptance的无恢复正常resume仍需正控保持；不要机械把所有resume.unwrap改成拒绝。
6. 新增Store直接调用四状态表驱动验证，至少真实restore流程造事实；claimed应在真实claim之后、finish之前验证。比较Task/event/approval/restore无变化。两恢复路由HTTP覆盖、重开后仍拒绝以及同Engine deferred_resume无残留由B/C分工验证。
7. 新Turn与resume不同：complete恢复后的显式新Turn若既有契约允许带Host恢复事实继续，应保持；partial/unknown/claimed既有新Turn阻断保持。本次严格化不得连带封锁合法新Turn或授权替换。

以上为只读范围建议。等待经理冻结行为、typed错误名称、HTTP码与新增文件所有权后再实施，不提前改测试求绿。