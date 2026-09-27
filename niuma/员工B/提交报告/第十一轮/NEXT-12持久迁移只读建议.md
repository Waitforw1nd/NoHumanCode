# NEXT-12 持久迁移只读建议

员工B/GPT-6-astra；2026-09-27；任务B-R11-PRE01，执行B-R11-PRE01-20260927-E01。固定源码主线ae86cf84b863bf42c2db85a46ff9aa22a1034fe1，初始status空。仅静态阅读产品/测试并写本报告与本人身份；未实现、未运行产品测试/SQL探针、未操作index或merge。以下是拟定方案，不是验证通过结论；旧B-R10已结项。

## 结论

建议schema13继续使用plugin_desired和plugin_management_requests，两表plugin_id CHECK同时由仅builtin.files改为固定IN ('builtin.files','builtin.shell')；其余列、类型、约束和key单列主键保持。每个插件独立revision，但管理key继续是跨插件全局唯一：原files key用于shell必须IdempotencyConflict，不能改成(plugin_id,key)主键或偷偷加前缀形成新命名空间。

升级仅保全files行/全部旧receipt并增加shell enabled=true/revision=0/updated_at=迁移时刻。true仅保持原本“可按Task权限和审批获得命令执行权”的能力；不得自动创建Task、批准审批、写command_network_ack、claim/spawn旧命令或替换grant。旧files=false不连带shell=false；两个独立desired不继承对方状态。

建议新增独立marker builtin-shell-management-v1（名称由经理冻结），保留builtin-plugin-management-v1原值及applied_at。不要把旧marker换名、重复写旧files updated_at/revision，或重制旧receipt时间。

## 当前准确入口与风险

- src/repository.rs:2866起：PLUGIN_TABLES为schema12唯一两DDL，apply_schema_12初始化files；verify_schema_12对完整DDL及单files行作严格比较。直接把旧常量改成双ID会令真实12校验失败，也会使真实13被旧单ID校验误拒。应保留独立v12定义/验证，再新增v13定义/验证。
- src/store.rs:8、73–98、230–240：当前SCHEMA_VERSION=12；启动预查12marker，后段current==SCHEMA_VERSION并无条件verify_schema_12。只改版本常量会拒绝正常12升级，也不能在13继续调用旧v12校验。
- src/plugin_management.rs:68–146：validate_plugin_id、desired_tx均只files；set_plugin_desired虽然update按id，但CAS前读desired_tx硬编码files。双插件时必须把id贯穿读取/比较/更新，避免shell CAS错读files revision；仍先查全局key再CAS。
- src/store.rs:1501–1567：claim_approval已使用Immediate事务，在审批绑定/Unknown或Claimed/取消/Approved/NotStarted/command网络ack/项目权限检查后，对write_file做files.enabled复核，再UPDATE claimed。shell检查应加在同位置的run_command分支，不能另开Store先读。
- 本次rg发现生产代码只有requests.plugin_id引用plugin_desired，未见第三表引用两管理表；这使保留foreign_keys=ON的子先父后重建可行。实际实现仍要检查外部引用/附加schema对象，不能把此静态扫描当任意用户修改库的保证。

## 原子重建建议：保持外键开启，不重命名旧父表

采用单Connection的TransactionBehavior::Immediate，整个12→13重建、数据拷贝、marker及user_version同一次commit。避免在事务里PRAGMA foreign_keys=OFF（SQLite事务内切换不会达到预期），也不要使用writable_schema修改CHECK。保持外键ON，无需defer_foreign_keys；以下步骤不存在持久child引用缺失parent的时刻。

1. 获取Immediate后重新读user_version与12/13 marker，确认目标仍是真12（并发开库若已13则转verify13返回）。在该事务内verify_schema_12、旧schema6–11必要校验和foreign_key_check；明确拒绝保留暂存表名碰撞，不用IF NOT EXISTS接纳未知旧残留。若旧两表有非产品定义的trigger/index、被额外表外键或view依赖，建议本片fail-closed并安全报告不支持，而非静默删除；这是建议额外策略，须经理明确，现行verify12没有检查全部附属对象。
2. 在事务中建两个专用暂存表（建议TEMP，显式指定main/temp避免同名遮蔽），用明确列清单复制旧desired和requests。暂存表不带FK；保存原rowid可避免不必要的观察顺序变化。复制前后比较总数和双向差集/逐列值，禁止INSERT OR IGNORE、OR REPLACE或去重。
3. DROP旧child plugin_management_requests，再DROP旧parent plugin_desired。child先消失，DROP parent不会对旧receipt作隐式外键删除/失败。两表均无AUTOINCREMENT，不需要重写sqlite_sequence。其他业务表完全不碰。
4. 用规范的v13完整DDL在原main表名创建parent，再创建child，child REFERENCES plugin_desired(plugin_id)仍指向稳定原名。相对重命名old/new父表方案，此法避免SQLite自动重写FK到旧名及DDL引号导致严格字符串校验漂移。
5. INSERT parent明确列（可带rowid）从暂存恢复旧files原值，再插入shell true/0与一次取得的迁移时间；不得UPDATE files。随后按明确列（可带rowid）恢复全部旧requests。FK始终开启，复制失败立即回滚。新shell没有迁移伪造receipt。
6. verify_schema_13完整DDL/PRAGMA结构、两行完整性、数据范围、foreign_key_check；对暂存旧files/requests作双向精确比较，确认旧值/数量全同。移除两暂存表。插入新marker一次，保留旧marker，设置user_version=13；可在commit前再断言marker/版本。任何步骤失败都让Transaction回滚，不吞错继续提交。
7. commit后才返回Store给Engine；Engine读取两份desired再按固定assembly构造，各按独立状态准入。失败启动不得默认回退shell=true；只有迁移明确新增行才默认true。

上述TEMP方案也须在实现测试中证明TEMP DDL随事务回滚/清理，避免同一Connection重试碰撞。也可用主库保留名无FK暂存表实现同一事务，commit前删除；不能留下发布可见第三张管理业务表。本报告没有实际执行任一SQL方案。

## v13校验与启动分流

v13两DDL只扩大plugin_id允许集合，key仍TEXT NOT NULL PRIMARY KEY，长度/typeof、enabled整数0/1、revision非负INTEGER、expected_revision<i64::MAX、result_revision=expected_revision+1、所有时间INTEGER与FK全保留。完整DDL核对必须同时检查PK/FK/CHECK/字段类型，不仅列名。数据校验要求正好files/shell各1行，无缺行/额外ID；receipt可零行，任一行必须引用这两行且保持字段范围。旧密文字节/Task JSON/approval v2/所有sidecar不解析重写。

| 开库事实 | 应做操作 |
|---|---|
| 新库或<=11，尚无13marker | 原迁移顺序到真实12（apply12仍仅files）；随后独立原子12→13。若13阶段失败可保留已成功的12，但不得半个13。 |
| version12、有12marker、无13marker | 先verify12，再Immediate内重核并apply13；12坏DDL/坏行不能因升级重建而被洗白。 |
| version13、12和13marker均有 | 跳过旧两表的verify12，调用verify13；原6–11相关验证仍全执行，不跳过整个历史验证链。 |
| version13缺任一marker、13marker配version12或更低、版本超支持范围 | 明确拒绝，不修补marker、不盲目重跑12或13。 |
| version13坏DDL/缺shell行/额外ID/FK孤儿 | 明确拒绝；不能INSERT OR IGNORE shell“修好”已声明迁移完成的库。 |

Store preflight添加13marker条件且拒绝提前marker；12 marker阶段接受12..=13，但必须按current分流而非一律verify12。正常schema13重开不调用apply12；否则会重新插files或因旧CHECK误拒。12→13前的version/marker检查应在Immediate内再读，不能仅依赖连接打开时旧快照。

建议verify12、verify13保留精确历史DDL，允许提取共同校验函数但传入明确版本允许集合/期望行数；不让v12函数“兼容接纳”v13，从而失去真实旧fixture识别能力。

## claim_approval的准确扩展点

保持当前顺序：绑定核对 → Unknown/Claimed拒绝 → 取消/Task running → Approved/NotStarted → run_command网络ack已Acknowledged → 当前项目权限/根身份 → 匹配工具的持久enabled → UPDATE审批claimed → commit。

在现行src/store.rs:1556附近将工具分支扩为write_file→ensure files，run_command→ensure shell，其余按已有语义。两检查均使用当前tx，不从新Connection读，也不在事务内部取runtime锁。建议共享ensure_plugin_enabled_tx(tx,id)并保留薄files helper供三读调用，错误仍typed PluginManagementError::Unavailable；缺失/损坏desired属于Internal，不冒称正常disabled。

runtime acquire持锁调用该Store claim：shell disable先提交则后claim不能产生claimed/spawn；claim先提交并成功返回lease则禁用允许该原lease按旧绑定完成。审批等待没有lease，approve不是execute；disable不得篡改status/ack。finish_approval/网络receipt收尾不得再以shell.disabled否认原合法claim。run_command依然要求allow_commands及原网络ack，不与files.enabled或文件write_scopes混为一项授权。不要在审批决定时新增自动启用/自动claim。

## 应适配的旧测试文件清单（实现派发需明确写权）

| 文件（源码根NoManCode/rust-app） | 现有准确受影响点 | 建议 |
|---|---|---|
| tests/approval_gate.rs | 1157/1442/1828删除12表与marker；1180/1445最终版本12 | 降到6/10等时额外删除13marker；最终版本用SCHEMA_VERSION；原业务断言保持。 |
| tests/checkpoint.rs | 669/807/837/1035/1107/1193低版本清理；680/1049/1173最终版本；744/769旧全表过滤 | 清掉13marker；最终版本更新。两表名称不变，旧快照排除清单无需再扩；不可新增排除旧业务表。 |
| tests/project_migration.rs | 31/200恢复schema10；102/223最终12 | 清13marker；保留真实10/11中间失败语义；最终版本改当前。 |
| tests/workspace_changes.rs | 560/1314低版本清理；563/1318/1352最终12 | 清13marker并更新最终版本，保留文件/密文断言。 |
| tests/project_context.rs | 223最终版本12 | 仅最终版本更新。 |
| src/engine.rs内部测试 | 2951恢复11；3006最终12；2636无WHERE更新所有desired、2665声称旧12、3086无WHERE查询单desired | 清13marker；真实12测试必须重建v12 DDL且删除shell，不可只改pragma；所有files测试SQL加明确plugin_id，防串改shell；新增13迁移专测。 |
| src/plugin_management.rs内部测试 | schema_12损坏测试与345版本12；250/381无WHERE更新desired；CAS/requests全局计数 | 拆真实v12输入损坏拒绝与v13重开损坏；files作用域SQL明确，保持全局key唯一计数与增加files/shell同key冲突。 |
| tests/plugin_management.rs | 管理trigger与startup/replay/单插件行为 | 保留故障语义，增加双插件独立CAS/重开/旧receipt；如plugins()列表由1→2，明确按冻结新契约适配而非删断言。 |
| tests/plugin_http.rs、tests/plugin_cli.rs、src/cli.rs内测试 | 当前固定单files目录/校验与响应fixture | 属传输契约所有者，不由B前置任务修改；经理依独立shell接口分配C，不能将旧files范围悄悄放宽。 |

tests/http_contract.rs:489已使用SCHEMA_VERSION，不需为了版本再改。tests/runtime.rs旧schema1是真手工DDL且最终用SCHEMA_VERSION，不自动扩大写权。tests/support/project_fixture.rs、network_fixture.rs本次只读：既有两表清理在五个测试调用点，可保持局部适配；若要统一新helper须单独授权，不能借重构跨写。

特别注意：NEXT-11旧schema12回执测试当前从最新Store建库后写固定时间值，并不是历史12 DDL生成器。schema13后必须明确测试名/目标：若验证真实12→13，用历史12 DDL与仅files行/旧marker；预期新增shell与13marker是唯一允许差异，不能还断言全部表毫无变化。若验证13重开原样，另测真正13并保持全表相等。历史报告不改写。

## 建议冻结的迁移验收反例

1. 真schema12 files=true/false且非零revision，至少两条旧receipt（含旧revision回放），全旧业务表含审批/工作区/Checkpoint/network/project/secret密文非空。升级后唯一新增shell true/0和新marker/version；旧files、全receipt、旧表逐列/字节不变。
2. 新库到13、13重开无写；缺/提前marker、PK改UNIQUE、FK改目标、CHECK放宽、类型改TEXT、缺shell/额外ID/孤儿receipt均拒绝。保留原version12损坏拒绝，禁止迁移“修复”输入。
3. 保持foreign_keys=ON，分别在新表填充/marker插入失败，确认原12 DDL和所有rows/rowid（若保留）、旧marker/user_version12、外键无损；无shell/新marker/暂存残留。marker失败使用schema_migrations触发器，原表trigger不会被重建吞掉；证明重试成功。
4. files旧key请求shell产生IdempotencyConflict且两desired/receipt全无漂移；各自新key相同expected_revision可各成功一次；同插件并发CAS最多一成功；回放不重协调，旧receipt内容不变。
5. 默认shell启用只装配能力，启动/reopen绝不launch/approve/claim/spawn既有Task、unknown或等待审批；files关闭时原有合法shell命令仍需审批+ack，shell关闭时files仍可独立工作。
6. run_command审批等待→disable→approve ack保留但最终claim零变化/零spawn；claim先赢→disable→旧lease真实执行与结果落库，不以禁用改为cancelled或unknown重试。进程/lease停止细节由D建议与经理总契约冻结，不在迁移报告替代。

本前置任务已完成；尚无NEXT-12实现授权与新工作树，以上验证全部未执行。下一第一操作仅向经理交建议供冻结契约/唯一写权/完整基线，不自行开始schema13编码。

## 经理后续方向补充：CommandOutcomeUnknown最小接入评估

接经理本轮补充，D拟提供crate可见command_execution::CommandOutcomeUnknown typed marker，表示已spawn后执行/回收结果无法确认，或owned worker通道丢失而无法证明结果。本段仍为只读建议，不是接口已实现。该标记应经ToolService/ToolLease的anyhow错误链原样保留，不能中途to_string或JSON化丢失类型；B用downcast_ref（支持context链）识别，不解析错误文案。

最小接缝是src/engine.rs:1651附近perform_tool的match lease.execute(task,args).await。目前任意Err都被转为JSON error、succeeded=false，再进入finish_approval_with_change，最终Store:1636把Claimed写Finished。这会将真实未知命令结果错误定性为已结束。建议在通用Err分支之前加guard：name为run_command且错误链含CommandOutcomeUnknown时，取既有approval_id，直接返回ApprovalError::UnknownResult { id }。不写tool_result、不追加role=tool、不调用finish_approval/mark finished、不修改审批decision/ack；保留已commit的Claimed。若理论不变量被破坏而缺approval_id，也应安全Err并保留状态，绝不能落回通用完成路径。

外层execute_or_wait:1475透传perform_tool Result，工具组循环:1342及恢复待执行组:1418均用?，所以该Err中断当前组和后续Provider回合；无需引入新的审批状态/协议字段。已写入的tool_start可保留作为尝试事实，不是完成回执。Task可按原外层错误收尾为failed（或原取消语义），但审批Claimed仍不可被作为可重试：Store.claim_approval:1524拒Claimed/Unknown，Engine.resume:1728同样先拒，Store.recover:1679把所有Claimed转Unknown。不得为方便用户重试把该事实改回NotStarted/Cancelled。

D必须冻结标记边界：明确spawn前失败、已wait得到明确退出状态、或已可靠kill+wait且结果契约已确定的情况，不能无差别当Unknown；反之通道消失不能默认“尚未执行”。保守Unknown适用于无法证明结果的边界，不携带命令/路径/任意原错到公开文案。该判定属于命令执行拥有者D，B只消费类型。spawn后的超时/取消能否得到可信终止结果，依D真实进程回收证据裁决；本方不靠runtime shutdown、drop Arc或字符串猜测。

建议验收补三个互补分支：

- 真spawn后受控lost-outcome/owned-worker失联，approval原Approved+ack保留、execution_state=Claimed、只有tool_start没有tool_result；Provider请求计数不增加、同组第二命令哨兵未触发；原Task resume/重复claim精确UnknownResult且零spawn。真Host恢复后Unknown，管理启用/重启不重试。
- 已确认普通命令失败（例如可靠收集的非零退出）仍按旧确定结果形成Finished与工具结果，不能为了修Unknown把所有错误都悬置。
- spawn前安全拒绝（未ack、disabled、权限/绑定不符）仍零claim或保持既有前置失败规则；不能被typed marker掩盖原因。所有实际派发先持久claim，unknown处理不得反过来补claim。

该最小分支不需要修改approval DTO/状态集合、数据库schema或网络授权模型，和本报告schema13迁移正交；需要总契约明确B的engine.rs接入、D新模块的唯一写权以及lib.rs导出所有权。当前无实现/测试授权，本次没有执行这些反例。
