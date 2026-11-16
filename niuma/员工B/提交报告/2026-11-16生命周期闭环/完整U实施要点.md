# B：完整U的后续原子切片

2026-11-16。经理要求当前schema17候选稳定，先完成本轮全套验证；本文是实施与审查要点，不是schema18已经落地或新的验收声明。依据[原NEXT-21U实施契约](../../../项目经理/任务/2026-11-01NEXT-21U-C统一数据域执行权实施.md)及本轮明确范围，继续使用现有`execution_authority`这一owner。

## 迁移顺序与旧库兼容

1. 先以首切片实际identity+原instance.lock取得维护排他，校验SQLite主handle。正常schema17旧库必须有真实可用的升级路径；不能复制C35对所有未初始化旧库返回`LegacyDomainRequiresControlledAdoption`的门。
2. 从旧C35逐项提取schema18必要元数据，先做与旧C35基线及11-16验收基线的三方差异，再逐函数迁入。只增加同一域绑定、单调epoch及recovering/active/closed等必要phase，不建第二owner、worker调度表或新后台服务。
3. 新库、正常旧schema17库、损坏/矛盾元数据和必须受控导入的情形明确分支。普通升级在原锁与实际绑定成立后进行，不把生产用户要求的迁移藏在test-only adoption/proof。若需要外部迁移能力，必须连同真实入口、失败保全、重入及数据校验一起交付。
4. 同一IMMEDIATE事务登记新epoch与recovering。恢复及必要校验提交成功后才发布active controller；溢出、身份失配、事务提交不确定必须封门。schema17→18迁移保留历史任务、审批、管理意图、配置、pending resume及账本顺序，不重新发出旧unknown。

每个候选必须先给旧库数据保全和普通升级正控，再测错误marker、失败回滚、恢复强杀与重新打开，不能以接口存在代替生产迁移通过。repository迁移/Cargo/lib仍由B独占修改。

## 同一能力贯穿所有入口

|入口族|要传递/核对的能力与边界|
|---|---|
|main、Store构造、--check、恢复、Engine构造|同域OS guard；维护与运行controller分离；epoch/phase生效前不可发布执行能力|
|Task/turn创建、resume、审批claim、tool结果采纳|事务内验证domain/epoch/phase，保留当前因果顺序和unknown不重放|
|provider/network授权、真实send及body/stream|同一原票从claim贯穿实际发送和响应消费；本地取消不证明远端副作用消失|
|文件工具、checkpoint/restore、spawn_blocking|后台closure和最终持久化持原票；在真正低层副作用/结果事务处fence|
|Git与Shell|沿当前整项票、generation、共享Supervisor继续；持久fence不得替代原handle/Job/readers收束|
|plugin/runtime管理、预算、worktree管理|审查分散Store impl及其IMMEDIATE事务；凡能授执行、重启或采纳结果必须核同一能力|
|配置、审批决定与只读查询|按实际作用分类，保留合法多连接和不授新执行的查询，不机械给全部SQL新建独占owner|

逐项查`store.rs`以外的`network_authorization.rs`、`budget.rs`、`plugin_management.rs`、`worktree_management.rs`、`host_configuration.rs`、`project_config.rs`和provider/runtime调用链。入口表须关联最终受约束的低层函数和事务，避免只有Engine外围check。

现有`ExecutionOperation`计数body和A `DomainLease`应直接扩展/携带同一域epoch能力；旧epoch不能仅因OS锁已交接而采纳迟到结果。能力不得可由用户domain/path/epoch整数直接构造。未知资源holder仍保留真实guard，epoch变化不能“结算”尚活的调用。

## 逐层验收与受控测试适配

- 实际域层：两个Store合法共享、第二controller/recover拒绝且不写活任务；两个真实Host同域竞争；--check遇活Host时schema/config不变；首次建库与别名/reparse/hardlink/替换竞争。
- 持久fence层：旧epoch、错误phase、跨域能力、epoch溢出/损坏、事务失败/提交不确定拒绝；拒绝前后表与事件内容保持相应事务原子性。
- 崩溃/重启层：OS guard取得后登记前、epoch提交后恢复前、恢复事务前后强杀；原process handle等待退出、原日志/DB保全，不把获取新锁当旧副作用结束。
- 业务层：Shell/Git普通成功、取消/输出排空/关闭/unknown、审批预算、流式脱敏、pending resume和文件因果完整回归。新迁移测试由C在独立tests交付；B只改自身内嵌fixture与核心。

旧测试若在活Engine上recover或在真实Host仍持锁时以Store::open读库，应分别迁到真实controller退出后的恢复、明确只读SQLite观察；不得删掉原对象/原事件断言或改成bool。第一切片六门禁先收口，再独立验证schema18；受测SHA/tree和日志分attempt保存。

## W3工件窄接口（另切片）

与D冻结的未来接口为Host binary `src/main.rs`唯一编译期消费 `option_env!("NHC_PACKAGED_WORKER_SHA256")`，值为64位小写hex。协议版本来自源码常量，不增加运行时环境/HTTP/CLI摘要、路径或协议选择。摘要不注入worker/lib/build.rs，避免工件自依赖；缺失或格式错误时未来bootstrap应保守拒绝，不降级信任同名邻接exe。

D工具先构建worker、冻结独立副本并hash，同候选/target/profile/features下只为Host构建注入摘要；复核实际compiler-artifact、冻结字节与源码后，仅发布新完整pair目录，不覆盖旧包。当前main未消费该token时，工具应在任何Cargo前拒绝真实打包。工具纯mock成功不等于Host绑定/运行期worker身份和协议已通过。

本轮不向main插入占位token来通过工具检查，不开放run_wasm，不把W3 bootstrap混入生命周期待验候选。后续启用须另交完整可信工件链、运行时身份/协议、同一执行权接线和故障矩阵。
