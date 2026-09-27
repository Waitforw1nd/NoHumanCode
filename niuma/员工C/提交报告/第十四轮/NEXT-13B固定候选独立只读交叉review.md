# 员工C：NEXT-13 B固定候选独立只读交叉review

2026-09-28，员工C/GPT-6-astra，同C-R14-01-20260927-E01附加只读任务。固定受审B `3a22df4d01fc5d9cf1649872b6aa4a65d7b0ba16`，仅在主仓git show/diff阅读worktree_management/repository/store/engine及必要project_config上下文。C源码74f9a09与构建freeze不撤回；未测试、未修改任何产品或B档案，不把后续b2a02ab结果混入固定审查。

## 新具体发现（已先消息交经理，最终定级由经理）

### N13-CB01：succeeded origin.git_dir缺少与该worktree admin关系的校验

位置：worktree_management::verify_persisted_rows succeeded分支、registered_worktree_root。对origin.git_dir仅validate_snapshot；随后强约束的是root/target/common/head/branch，没有验证git_dir属于该linked worktree的admin路径或与common的关系。具体损坏输入：保持所有列/receipt/plan/root/common/head/branch不变，仅把project_worktree_origins.identity.git_dir替换为同库source_root的合法RootIdentity JSON。它本身是合法identity snapshot，当前静态校验链不会因该错误admin归属拒绝重开，registered_worktree_root也不检查此字段。

影响为schema14承诺的持久origin严格校验缺口；不是已经证明可执行越权，也不是本轮运行过数据库注入。建议B加单字段反例并在不依赖可变外部当前状态的前提下绑定origin admin/common/该target关系；经理判断是否并入既有B01。

### N13-CB02：未决副作用busy测试夹具在到达断言前即使用不存在列

位置：worktree_management::tests::n13_cancelled_task_unresolved_restore_or_write_blocks_claim。首个setup使用 `INSERT INTO runs(id,value)`；固定Store的runs为id/title/created_at/kind，没有value。接下来task_project_bindings等fixture也需按实际DDL核对。因此该测试代码的六状态循环存在，不足以证明B01所有未决write/restore/outcome路径动态通过；它会先在setup失败。

这是固定3a22的具体证据缺口，不否认ensure_project_quiescent_tx新增SQL的生产语义。未执行该测试；后续B候选或实际专项修订应以对应新SHA与实际日志关闭。

### N13-CB03：inspect的全Project在线前置阻止身份变化诊断形成DTO

位置：Engine::inspect_project_worktree在调用D::diagnose前执行worktree_inventory()?.roots()?；ProjectInventory::roots对全部Project执行root.verify及fs::canonicalize。具体触发：已有unknown操作后source根移走/目录身份改变，或一个无关Project离线，inspect在D diagnose之前即返回RootChanged/Unavailable错误，无法产生契约声明的IdentityChanged/Unavailable观察。status/lookup仍可读，但W10身份变化inspect路径到不了WorktreeInspection。

建议将创建所需“全部Project必须在线”与诊断所需持久快照排除列表区分：诊断上下文构造只验证安全snapshot，实际身份变化交D diagnose分类，保持无DB修改/无Git副作用。不由C擅自决定合同是否允许此场景仅返回HTTP错误；已交经理明确判定并给B/D落点。未运行身份替换实验。

## 已核对且未发现新阻断的主路径

claim_worktree使用Immediate，key回放优先，比较完整ProjectInventory、source revision/root、plan身份、成员busy与预约，common活动唯一索引及target/branch约束在SQLite保护内；新普通注册遇claimed/unknown拒绝，避免盘点后加入成员。B01已知成员精确集合/requesthash/plan-origin新增检查未重复泛化派项。

Engine持久claim后同步spawn owned worker，中间无await；caller取消丢JoinHandle不取消worker。Git成功后Project/config/root/origin与receipt成功/释放预约同一Immediate提交；finish_worktree先核state/revision，其他Host recover已变unknown时旧worker只能读回而不得成功/释放。无法落库按unknown保守处理；recover只claimed→unknown，重复recover不漂移、无Git、预约保留。

反向准入检查位于各Store事务；旧回放、绑定/封存优先，拒绝/取消和已领取finish可继续收尾。schema14迁移先严格13，规范DDL/唯一索引/FK及持久行校验后同事务marker/version；重开不会通过缺marker自行洗白。本轮未运行并发开库、竞争、迁移或崩溃，不把静态路径判断写成动态证明。

## 当前状态

交叉review完成，三项已先同步经理，无C源码/构建状态变化。只写本人本报告及身份追加。下一第一操作优先接经理新的真实组合并恢复C专项；本报告不产生第二执行者或B/D写权限。
