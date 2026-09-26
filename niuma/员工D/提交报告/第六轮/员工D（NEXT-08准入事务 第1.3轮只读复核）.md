# D-R6-01 第1.3轮配置准入事务只读复核

2026-09-27，员工D GPT-6-astra，同执行ID D-R6-01-20260927-E01，经理有界派发。固定输入c793c797bb471168543d950ccb4b649aab70e9f4，所有证据git show/git grep该提交；不读持续工作区作最终事实，不跑测试、不改D3452c67冻结产品。

结论：限定Task创建、审批claim、network admit、旧Turn快照与grant继承线，未发现可定位的新阻断。Store::configure_project虽为pub，但这些持久准入并非仅依赖Engine gate。

- project_config.rs:468–489 configure_project获取Store DB mutex并开启SQLite Immediate事务，在事务内读当前revision、CAS检查、规范完整配置及更新commit。
- store.rs:566–606 Task创建同样Immediate事务，先处理历史幂等回放，再写领域记录；602–603在同事务绑定项目与网络。project_config.rs:316–336重新读当前config并要求expected revision及permits，写sidecar后再验证。撤权事务先commit则创建事务看到新revision/ceiling，失败会回滚本次领域记录；反向先commit的任务仍在后续副作用准入被当前ceiling检查。
- store.rs:1461–1516 claim_approval同Immediate事务，在1510 UPDATE claimed前的1509调用validate_task_tx(...,true,false)。project_config.rs:301–306读取当前config并检查整份Task实际权限；无仅Engine gate的间隙。approval_binding在store.rs:2078–2089还对照durable workspace/scopes/command/tools与工具策略。批准是另一事务事实，不能替代claim时重验。
- network_authorization.rs:864–917 network_admit在Immediate事务内checked_authorization后才INSERT claimed和commit。checked_authorization:636–640核Task当前ceiling与项目，643–647取Task原具体grant及digest/完整subject；663的current_subject在298–309再次读取项目config并检查allowed_route_ids。无Task的models/check也经current_subject当前ceiling。故公共Store配置更新先赢时，后续network claim不会沿Engine旧预检发送；claim先赢则属于已持久在途事实，符合契约。
- store.rs:2332–2338 save_task_tx拒绝spec/route/workspace/created_at变更；2046–2055 candidate_continues_snapshot要求后续Turn保留前序route、workspace、tools、write_scopes、allow_commands等。882–883先核前序当前ceiling，913–921在当前事务绑定新Task并inherit_task_tx。配置变宽不会直接扩大旧快照。
- network_authorization.rs:705–721 inherit_task_tx从新Task项目构造subject，再checked_authorization(...Some(previous))读取前序具体authorization_id，而非获取当前任意active grant。checked_authorization核前序项目==新subject项目并核完整grant.subject、digest、状态和当前连接revision。固定候选当前行为是复制前序具体grant，跨项目继承被拒；这不是新Turn grant验收目标，见下方经理补充裁决。
- 补充同范围：store.rs:1081–1088 claim_restore也在Immediate事务内当前ceiling校验后持久claim。公开旧commit_turn_bundle使用None仅形成无sidecar历史对象；后续validate_task_tx在project_config.rs:286拒绝legacy_unbound，未发现由该兼容入口获得native副作用准入。

本结论是固定源码审查，不是竞态实测；B/C仍须提交撤权先赢零claim/零网络，以及已claim真实在途保留、跨项目grant、扩权不改旧Task的正式证据。未扩展到命令沙箱、外部文件系统TOCTOU或新无界审计。

经理补充裁决（本轮收到）：NEXT-07允许completed Session显式新Turn绑定兼容当前grant，NEXT-08未取消。B正在修append为新Task绑定原项目当前有效grant；旧Task/resume仍保留旧具体grant，原project/root/route/spec继承及unknown/封存阻断保持。本次对c793的grant描述仅为已观测源码事实，不把复制旧grant作为验收要求，新修订待另行固定审查。

当前D产品仍冻结3452c67，经理已通知组件review0.1及9对hash核验。下一第一操作等待经理明确固定测试证据或后续有界复核；不自行继续改产品。