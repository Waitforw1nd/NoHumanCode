# NEXT-18 D 第一静态候选审查

2026-10-31，项目经理。候选 `c6b46277dcf22936b5f546b58bd7a692bf15484f`；NoManCode `fb5615d8e2cd6f8222a28869ac1a813f11a5ab6c`，Rust `1079f19f82a999634bc5ad0f9876f13c52427b1e`。已读新测试、support helper、两旧runtime测试增量及历史夹具差异；本人已明确source/index/unmerged空、未构建/运行。

结论：NEEDS_FOLLOWUP（静态测试建设中），不作产品通过。13文件在授权范围；旧夹具删除17marker或重建真实两ID结构的方向合理，历史目标版本没有批量改写；新保全例包含负时间、rowid与opaque BLOB。以下为完成原M矩阵所需的接续，不要求为了“独立”复制B的所有单元测试。

## D18-R01：故障拒绝须绑定目标原因

P2。runtime_management_schema 的 migration_failure_at_marker_is_atomic 仅assert Store::open is_err，再断言回滚状态。如果实现因前置结构检查失败，仍可能通过，无法证明执行到了注入的marker故障。应确认目标注入确实触发，例如明确检查该错误来源/受控标记或独立回调计数；并在移除故障后证明同一数据库可正常迁移。

其他损坏拒绝用例也需防止因无关夹具损坏通过：验证合法初态、预期破坏点、拒绝原因分类和持久状态。保护性拒绝不能泛化为所有open错误都证明目标不变量。

## D18-R02：跨Store准入需正反配对和实际账本

P1，M07/M08未完成。新增Provider旧prepared例通过真实网络admit拒绝，但fresh positive只用返回Ok的闭包，未证明fresh revision可通过相同持久网络/预算事务。补有效相同夹具的真实准入/完成或项目route check，核实际mock调用和最终账本；旧prepared拒绝既核network_calls，也核预算账本未增。

Agent旧lease例目前只核计数；至少实际调用旧lease策略验证仍可用，并在B隔离屏障到位后完成两独立Store的准入commit与disable双向交错。不可只运行一次无屏障竞争或把本地Mutex当跨Store保证。数据库失败、commit失败不得保留新增lease或使用cached true。

## D18-R03：崩溃现场需可定位到具体阶段

P2，证据准备。现有迁移child确有kill+wait和两次新进程重开，但最终runtime.db在恢复后被继续升级，不能独立审阅kill后的旧版本现场。增加每阶段明确元数据：阶段、child PID/执行身份、signal内容、kill/wait结果、观测user_version/marker，以及在任何Store恢复之前保存可校验的数据库现场快照。原现场与后续恢复都保留，必要WAL/SHM一并如实记账，不能从最终库反推当时版本。

不要求修改旧超时/并发；只给新增测试补清晰阶段记录和失败现场保留。单纯Connection读取可能触发SQLite恢复，报告需区分原始kill后字节与读取后可查询快照，不冒称同一时点。

## D18-R04：完成尚待探针的原契约场景

P1，范围未完成。继续补管理commit前后真实kill与两次重开、transitioning/draining的replay先于Busy、新key不消费与同态写、零激活计数、生命周期失败/溢出与guarded析构重入、permit提交后失败补偿，以及M02剩余CHECK/PK/FK/非法类型/高水位/staging碰撞等反例。可复用已有效旧组件场景，但须说明新持久路径是否实际经过它，不能仅列旧测试名。

与B直接对齐最小cfg(test)探针，生产共享文件与lib仍由B唯一写入。核心未完成的草稿不可当固定依赖；缺探针时先完成其他可独立静态编写部分。

## 执行

允许D在原授权文件范围解除source/index冻结继续静态补强并提交新候选，no-build保持。原候选/报告保留。阶段代码完成后与B本人冻结，由经理串行整合真实文件，B接lib，再固定组件候选与放行。尚无M条目动态通过或联合验收。
