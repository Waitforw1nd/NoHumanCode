# Store 共享开库固定复核

日期：2026-11-16；A 独立只读审查。旧开库基线 `668c6b89dda33260f900de38463cd71677e4ab3c`；修复候选固定为 B `62ae726fb43bcd323ffe5b90254d690dcd2a4533`。A未修改Store/repository/budget，未运行Cargo或真实测试。

结论：**62ae726f 的 verify_initialized_schema 补齐旧开库在当前schema17下应保留的只读校验；Store::open initialized分支与open_shared调用同一helper，本差异未见静态阻断。预算四个旧损坏用例和新增open_shared断言尚未动态验证。** 经理当前full-1受测HEAD冻结，不能把其结果归给未合入的修复。

full-1的source-before记录HEAD `e6ddea7e2468f79e6fb295b329cac163a2d354ed`。`.local/lifecycle/integrated-full-1/06-test.log` 的80至105行显示四项原 `Store::open(...).is_err()` 断言失败：aggregate_overflow、missing_covered_row_and_single_sided_terminal、request_digest_and_coverage_high_water、request_sql_scope_and_policy_history。原因是旧路径每次都会到达schema15预算验证，而新initialized捷径只查schema17的plugin/runtime部分；open_shared原先也完全没有持久校验。这不是预算fixture应放宽的断言。

## 每次共享开库必须调用的集合

|范围|固定候选的实际调用与旧边界|
|---|---|
|版本/迁移标记|user_version精确等于17；schema6至17的12个migration marker逐个存在。initialized不是数据库仍正确的证明|
|schema6|verify_schema_tables(Complete)与verify_event_columns；完整列/FK/索引/唯一性及身份碰撞校验，不创建缺失索引|
|审批/文件/检查点|verify_schema_7、8、9；含原审批持久记录、列/DDL/索引/FK校验|
|network/project|verify_schema_10、11；恢复网络DDL、network_one_active索引、FK，以及项目表结构与selection检查|
|plugin/shell/runtime|verify_schema_17；覆盖当前四个builtin、desired/receipt、非法附加对象与迁移暂存名。不能对schema17再调用只接受旧插件数量的verify_schema_12/13|
|worktree|verify_schema_14，包含worktree_management::verify_persisted_rows和FK，保留receipt/plan/member/origin离线结构校验|
|budget|verify_schema_15，包含budget::verify_persisted_rows；恢复策略历史、digest、覆盖边界、network/budget配对、汇总溢出等原校验|
|Host config|verify_schema_16，包含host_configuration::verify_persisted_rows；恢复revision/请求receipt及其封存fingerprint的原校验|

## 只读与不可改边界

两入口都先经原authority.connection绑定实际数据库身份，设置连接级foreign_keys/busy_timeout，然后在一个deferred事务中完成helper并commit后返回Store。此处“只读”指不改持久schema/业务行；并不是将返回的Store连接改成SQLITE_OPEN_READ_ONLY。schema10/11为比较DDL构建的临时内存数据库不写目标库。

固定helper不调用 `verify_current_schema`，后者虽然名为verify却会执行CREATE UNIQUE INDEX；改用Complete检查后，共享开库遇到缺失索引直接拒绝，不偷偷修复。首次开库既有迁移与允许的旧索引修复原样保留。

不得在共享校验混入apply_schema、recover、默认设置写入、network配置刷新、票据结算，或重置initialized/admission/unknown。不得新建第二owner、放开活controller下recover、改变schema17/JSON。校验失败不重写旧receipt，也不阻止已有持权工作沿原路径收束。

覆盖范围与旧行为一致：verify_schema_10本身主要验证结构/索引/FK，预算校验另读关联network receipt；本修复不新建全量network语义审计。旧开库没有统一对legacy settings调用新的Settings::validate或解密所有旧route secret，不能借此改变旧配置兼容政策；Host配置历史receipt原有的只读fingerprint解封校验仍须保留。

## 固定修复复核及未验证项

已用git show/diff固定读取62ae726f，调用集合与上述一致；open_shared和initialized分支没有旁路。四项budget损坏测试增加open_shared拒绝断言而保留原Store::open拒绝。相同提交的budget crash夹具增加活controller下recover拒绝且receipt不变，drop后再recover并重建Engine，原无重复执行断言保留。

A已将集合、只读边界和最终静态结论直接发B。固定差异diff-check通过；没有执行动态用例。经理下一轮验证四个旧budget损坏case及open_shared新增断言，之后相关schema/network/runtime/config回归才能形成动态结论。
