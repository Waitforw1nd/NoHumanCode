# NEXT-13工作区持久契约只读建议

2026-09-27，员工B/GPT-6-astra，B-R12-PRE01-20260927-E01。新只读任务，不恢复B-R11。已读经理NEXT-13前置登记，实核main b8fa6dd19229a97c80b0657694b77308b7109993，NoManCode191687949486545e22ed0769eaa32aee5d6faf6a、Rustae885d552659810753d00ff796dd542249d21197。index空，经理管理文档dirty合理，无产品修改。本报告只读代码建议，未测试、未执行Git副作用、未修改产品或操作暂存区。所有源码路径以仓库根为准。

## 建议结论

首片选择显式创建linked worktree，并登记为独立Project；用户随后显式配置该Project、确认其网络授权、启动一个Agent的新Run/Session。shared模式就是现有Project根上的现有start_in_project，不新造workspace重定向。同一Session及其新Turn固定同一Project/根，不迁旧Task、不迁grant、不把原Project选择偷偷切换。第一片不承诺复制当前未提交修改、自动合并、删除worktree或完整仓库隔离；linked worktree共享Git对象与refs，独立的是检出目录与index。

此建议新增“创建及可审阅登记”闭环即可，不把创建、配置、网络批准、启动四个不同授权事实揉成一个隐式动作。若产品要求一步创建并运行，仍必须在持久操作成功后使用独立明确请求启动，不能创建失败/unknown时自动运行。目录不是复制生成的伪worktree。

## 当前准确依赖

| 路径/函数或表 | 当前事实及影响 |
|---|---|
| NoManCode/rust-app/src/project_config.rs:237 root_tx、247 workspace_tx | Project唯一根来自project_root_bindings，RootIdentity.verify并对比projects.root_path。不能让同Project临时返回另Session目录 |
| 同文件:280 validate_task_tx、310 bind_task_tx | 核Task→turn_tasks→sessions/turns→Project；Task binding根须等于Project根，Task.workspace canonical须相同；permission_digest/config revision同时约束 |
| 同文件:380 Store::register_project | Immediate内先幂等回放，再capture根、与所有Project比RootOverlap，拒绝重复/嵌套；登记默认ProjectConfig与root binding及registration key。linked worktree建议放源根外的兄弟目录，否则OverlappingRoot会拒绝 |
| 同文件:468 configure_project、497 select_project | 分别revision CAS；创建新Project不代表选择它或复制其权限 |
| NoManCode/rust-app/src/engine.rs:841 start_in_project、869 start_project_locked | 使用显式Project/config revision，workspace取Project根，新建Session/Task。含命令Task限制单独运行，不能以worktree创建顺手放宽 |
| 同文件:1045 send_chat_turn、1053 send_chat_turn_locked | 新Turn核Session归属/最新完成前驱，再validate_project_task；新Task复制previous.workspace及spec。新Turn不是切根入口 |
| NoManCode/rust-app/src/network_authorization.rs:298 current_subject、638附近校验、684 bind_task_tx | route授权subject携project_id；Task绑定已有授权id/digest。新Project即使相同route也不能复用源Project grant；旧Task保持原grant，新Task按现行契约绑定本Project当前授权 |
| NoManCode/rust-app/src/store.rs:1543 claim_approval、1690 recover | 审批绑定/状态/ack/项目根及持久工具准入同Immediate，claim先提交才副作用；recover把claimed变unknown。新Git操作应借鉴顺序但不用伪造工具审批Task |
| 同文件:1165 claim_restore、391 resume_task及ensure_resume_unsealed | restore先claim；恢复事实/Checkpoint封存原Task。worktree创建不应绕过封存或“搬家后续跑旧Task” |
| NoManCode/rust-app/src/engine.rs:2100 restore_locked、5432 restore_checkpoint；checkpoint.rs::Manifest/source_digest | restore仍核原Task根/写范围/当前after；manifest含task.workspace，before密文与workspace/binding digest绑定。Git创建不是Checkpoint恢复，也不能把Shell/Git修改说成有before快照 |
| NoManCode/rust-app/src/workspace_identity.rs:90 capture、99 verify | 目录物理身份与路径策略，verify是观测而非持有句柄的沙箱。不是Git common-dir/worktree登记身份；Git身份需D另提供 |
| NoManCode/rust-app/src/repository.rs:338起旧模型表、文件顶部schema11表、2973附近schema13 | projects根；sessions/turns固定project_id；project_configs/root_bindings/task_project_bindings；当前schema13。建议追加schema14新表而不改旧JSON/旧FK/根绑定，保留6–13验证 |

## 两种方案成本

| 项 | 独立Project linked worktree（建议首片） | 同Project每Session workspace |
|---|---|---|
| 根模型 | 复用一Project一根，新Project绑定新物理目录 | 新增workspace实体/Session binding，重写workspace_tx与validate_task_tx的Project根等式 |
| 旧Task | 完全保持 | 必须versioned sidecar兼容旧Task→Project根；不得重写旧Task.workspace |
| grants | 新Project显式新授权，现行机制即可 | Project授权可能覆盖新增根，必须决定是否根绑定授权，不能暗中扩大旧grant；影响subject digest/Task bind |
| Turn/恢复 | 新Session在新Project，旧Turn/Checkpoint原封不动 | send_chat_turn、resume、restore、checkpoint与scope冲突都需workspace身份接续与迁移 |
| 兼容成本 | 新operation表+新Project来源关系，较小 | 新Task binding版本/Session根映射/根查询契约/迁移和错误优先级的大面重构 |

不能把同Project方案包装为“只加mode字段”。后续真需要逻辑Project统辖多个workspace，可设计独立Repository/Workspace层并显式迁移；首片独立Project来源关系保留演进线索，不把新Project伪装成原Project。

## 建议持久模型/API（均待经理冻结）

建议schema14 marker `git-worktree-operations-v1`，只追加：

- `git_workspace_operations`：operation_id主键，idempotency_key全局单列唯一（本接口域），request_hash、source_project_id外键、source_config_revision、source_root_identity与Git common-dir物理身份快照、target_parent_identity/目标规范路径、新branch名、输入ref及一次解析固定base_oid、新Project预分配id、状态、revision、安全cause_code、created_at/claimed_at/finished_at。字段精确DDL/严格类型/CHECK及status对应nullable关系冻结后实现。
- `git_workspace_locks`或operation partial UNIQUE：对canonical common-dir身份最多一条claimed/unknown当前操作。不能只按source_project_id锁，因为不同Project可能同一Git仓库。unknown继续占槽；跨进程依靠SQLite，不靠Engine mutex。
- `project_workspace_origins`：新Project唯一FK，source_project_id、operation_id唯一FK、common-dir/worktree Git身份、base_oid、branch。没有此行的旧Project即shared/外部登记，不反推历史Git事实。

目标路径与branch额外预约键避免不同仓库/操作撞同目录或同refs；请求key先查再做current revision/状态检查，回放仅返回同操作事实，不spawn/reconcile。异参同key精确冲突。新请求要expected source config revision和operation revision CAS；不得把config revision当operation revision共用。迁移先验证真实13、Immediate内建表/verify/marker/version一次提交，13原两插件行、receipt、所有业务/密文字节保持。并发开库/异常回滚必须动态测，不能拿CAS测试代替。

建议Host显式接口（命名供C/经理统一）：CreateWorktreeProject(source_project_id, expected_config_revision, target_path, branch_name, base_ref, name, idempotency_key) → operation receipt；Get/ListWorktreeOperations；InspectWorktreeOperation只读诊断；显式FinalizeVerifiedWorktree(operation_id,expected_revision)仅在用户确认与完整证据验证后完成登记。不要给未知操作自动retry按钮。最小拒绝码区分invalid/ref-invalid/root-changed/overlap/revision-conflict/idempotency-conflict/busy/unknown/git-unavailable/registration-conflict，响应不外露凭证或原始stderr。

Store建议私有`claim_worktree_operation`、`finish_worktree_operation`、`mark_worktree_unknown`及只读查找；Engine orchestrate，D只负责Git探测/受限命令执行/证据。当前register_project自己持锁开事务，**不能在新operation finish事务内直接调用它**；应由同一owner提取transaction helper，复用名称/根重叠检查与默认权限，同时原公开register行为不变。finish须将Project+root/config+origin与operation succeeded一次commit，不能先登记后另commit receipt导致半完成。

## 副作用与未知恢复闭环

1. 只读预检：源Project/root/config、Git仓库/common-dir/目标父目录身份、目标不存在及不嵌套既有Project，合法本地commit/ref解析为固定OID、branch可创建；不stage/commit清工作树，不从用户输入拼shell命令。允许哪些Git ref/dirty源状态由D冻结；按固定OID创建不会包含未提交修改，须用户可见。
2. Immediate领取：先key回放/异参拒绝，再核source revision/root快照与目标预约、common-dir单当前操作；持久写claimed与revision后commit。不能事务锁跨await。Host gate串行只补同进程，不能替代持久约束。
3. 副作用前复核身份，D执行显式worktree add到约定目标/新branch；进程有deadline/有界收尾。确定尚未spawn且零变更可not_started/failed；只要spawn或Git非零后可能部分refs/目录变更，不能仅凭退出码伪造rollback，一律unknown除非有完整可验证事实。
4. 成功需核Git登记、common-dir身份、目标RootIdentity、branch/ref/初始OID、路径/Project重叠，随后原子登记独立Project+origin+succeeded。退出0后DB commit失败仍unknown/claimed，绝不自动第二次add。DB不可写时保留原claim，下次recover→unknown。
5. startup仅claimed→unknown，不执行Git、不删目录、不换源Task/grant。诊断可观测“已生成”“未见”“冲突/部分”，**观测不等于批准重试**。为了闭环可允许显式“验证已有产物并完成登记”，它不再执行add；须CAS unknown revision、目标所有权与Git证据全匹配。若无法证明归属，继续unknown并阻止新同仓库操作；首片不引入强制forget/unlock或自动清理。
6. failed只有可证明没有残留才释放预约；succeeded释放运行槽但保留永久目标/branch来源事实，其他操作仍核Git真实当前状态。外部Git可能变化，SQLite锁不能阻止用户或其它Host数据库修改；副作用前后复核，不宣称OS事务/完整隔离。

成功登记默认无权限，显式configure→network preview/confirm→start_in_project才产生新Task。来源关系只作审阅，不继承旧Agent身份、Conversation、Task、恢复记录。旧Project仍可shared；删除worktree/迁移旧Session/合并回主线不在首片。

## 验收建议

- canonical13非空迁14全旧表/rowid/密文/插件intent/receipt保全，缺marker/坏DDL/孤儿/并发open/填充失败拒绝或回滚。
- 真Git临时仓库创建linked worktree并登记，目标内真实文件工具/经审批ack命令，shared目录不变；证明独立index且共享refs的边界，不只看目录名。
- 新Project默认权限拒绝、新grant所需；旧Task/root/approval/grant/Checkpoint逐值不变，旧Session新Turn仍原根；相同route不能跨Project复用grant。
- 同common-dir的两个source Project竞争最多一claimed；同key同参回放零Git、异参冲突，目标/branch竞态与revision stale零副作用。
- 三个真实进程kill+wait断点：claim后spawn前、Git已创建后DB前、登记事务中失败；恢复unknown、不自动add/清理，不重复Project。只读诊断重复零漂移；显式verify-finalize只登记一次。
- 根/父目录替换、目标预存在/嵌套、注册时路径冲突、Git部分失败/超时、stdout未知、SQLite失败均安全拒绝/unknown；不能用happy-path DTO测试冒称这些动态事实。
- 新Project内Checkpoint仅覆盖原有write_file before机制，Shell/Git修改不假装受其保护。已恢复旧Task仍sealed。

## 唯一写权建议及待裁决

B建议独占新`src/worktree_management.rs`（DTO/Store契约）、store/repository/schema14、project_config事务helper、engine编排及对应持久/恢复测试。D独占新Git执行/身份模块、workspace_identity必要最小扩展和其真实Git组件测试；C独占CLI/server路由与HTTP/CLI集成测试。lib.rs模块声明必须指定唯一写入人（建议经理集成或D），不能B/D各写。domain若需新增类型只给一人；本方案尽量将DTO留新模块避免共享碰撞。网络/approval/checkpoint旧模块原则只读，确需改需另冻结，不以岗位职责泛化权限。

经理待定：是否批准独立Project用户流程；目标目录显式输入还是Host生成；是否仅新branch+固定本地commit；unknown显式verify-finalize是否纳入首片（建议纳入，否则创建成功但receipt丢失只能长期悬挂）；common-dir单当前操作范围。D需提供可靠Git身份和产物归属证据，无法证明时应缩小finalize能力，不能为了可恢复而认领任意现有目录。

本报告未实现/未测试，建议API/表名不是已存在事实。下一第一操作等待经理冻结完整契约/基线/独立工作树及新实现执行ID，不自行开工。