# NEXT-18 D 第二静态候选审查

2026-10-31，项目经理。实核候选 `1cbca8662d3695b174018925340172f12b4f469a`，NoManCode `2e4f28fb5214f423794bb5ea87526fcd88d004c0`，Rust `296ae5f23f5d5b78c7af98e35fb24c14e6a5171a`，status/index空。已读相对c6b46277的五文件完整增量、迁移崩溃夹具全文、当前管理测试全文，并对照B活动草稿实际接口。

结论：NEEDS_FOLLOWUP。已补部分可进入后续动态验证，但尚有原M04/M07/M09/M10范围内的关键证据缺口；以下不是新增产品需求。no-build保持，可解除原授权文件source/index冻结继续静态补证，不改B生产文件。

## 已补部分

D18-R01目标marker注入现有明确错误来源断言，并在移除trigger后同库成功；历史损坏用例增加正常控制及目标错误原因。D18-R02旧Agent lease实际调用策略、Provider fresh改为真实route-check和预算/网络账本；双Store双方向事务屏障覆盖先后顺序。D18-R03 kill+wait后先保存原始DB/WAL/SHM字节/hash，再连接SQLite观察并保存queryable快照，保留九迁移阶段及两组件管理提交前后现场。D18-R04补Busy回放、零activation构造器计数及启用正控、代次耗尽、准入DB失败和permit提交后补偿。

零activation计数与持久Engine启动是互补测试，报告正确区分两者，不得改称同一端到端注入测试。上述均未执行，真实阶段信号、退出码、原件和恢复结果仍需动态核验。

## D18-R05：管理事务失败与回调失败的结果语义

优先级P1，验收缺口。当前DB失败测试覆盖准入，真实kill覆盖崩溃；两者均不能证明同一进程管理事务失败时不改变本地admission/generation，或提交成功后callback失败仍返回receipt/current。

在两组件set_runtime_enabled路径分别验证：

1. 通过目标receipt插入失败或等效确定性事务故障，使管理事务回滚；比较desired、receipt行、完整本地状态/代次与失败前一致，故障解除后原key同参可成功。证明发生目标故障，不能只断言任意is_err。若注入点在提交前语句，应如实称事务写失败而非真正commit失败。
2. 利用已有自定义PluginHost/故障回调，走新持久管理入口触发生命周期错误或panic；断言方法返回已提交receipt、持久意图与非收敛状态，原key回放不重复回调，新enable不能抹去生命周期失败。旧local-enable/disable测试不能独自证明这一新返回语义。

若需要真正commit失败，优先现有SQLite commit_hook或可复用配置；若当前依赖不支持，向B提出cfg(test)最小探针依赖，不能擅改依赖或把语句失败称作commit失败。

## D18-R06：同值revision改变也使旧prepared失效

优先级P2，M07明确边界。当前false→true跨Store测试验证revision变化，但不能单独约束“只检查最终enabled”的同值管理行为。增加另一个Engine在保持true时以新key提交true、revision递增；旧prepared最终真实network/budget准入拒绝且无新账本/预算，新的prepare经真实准入成功。可复用现有fixture，不新增虚假Ok closure正例。

## 接续与证据

原两候选与报告保留；完成增量固定候选、本人clean及报告后再交经理。可保持既有测试/timeout/并发不变；不要求为静态审查提前运行组件。B固定后可先进行B自己的限定组件，不因本补证等待而将D未执行项标为完成。最终完整组合与五门禁仍未开始。
