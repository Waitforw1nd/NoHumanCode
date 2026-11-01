# NEXT-22 持久调度与 Team 核心事实盘点（C-R33）

2026-11-01；员工 C。固定 main `5cc0bddb352d907f4954d46c6ca49ba85eb63583`，产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58`、Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`。以下行号均指 `NoManCode/rust-app` 的固定 Git blob；为只读源码分析，非本轮动态测试结果。

## 发现（按影响排序）

1. **重启后 DAG 不自动续调。** `src/main.rs:125` 调 `Engine::new_recovered`，`src/engine.rs:783-788` 仅先执行 recovery 再组装 Engine；`src/store.rs:2254-2290` 把所有 queued/running 旧任务变 interrupted，保留 completed 等终态，收敛 claimed 网络调用和审批/工作区未决状态（`:2266,2292-2305`），不会扫描就绪队列启动任务。例：A completed、B queued 且依赖 A 时 Host 被杀，重启后 A 仍 completed、B interrupted，除非显式对 B resume，否则永远没有新调度；同 key 请求只返回旧 Run（`engine.rs:1479-1491`），并不续调。不能因这个现状断言自动重复执行：当前恢复不自动重发。
2. **失败传播后恢复上游不会唤醒下游。** `engine.rs:1695-1697,1855-1906,1908-1945` 一次为每个节点启动内存任务，依赖失败/cancelled/interrupted 后下游报错，自身变 failed。例 A→B→C：A failed，B 发现后 failed，C 再发现 B failed；用户只恢复 A 即便成功，B/C 仍终态，不会自动重新入队；必须分别显式处理。`cancel` 只取消指定的内存 active 和待审批（`:2306-2312`），依赖错误经轮询传播为失败，不是持久全图取消。无法由这些代码证明正常生命周期下一定有活跃任务永久轮询；终止状态之外若出现不能识别/未完成状态则该循环仅持续等待，并非有界诊断。
3. **显式 resume 不是幂等任务重放或 Team 续调。** `engine.rs:2217-2304` 接受非空消息并对非 active 任务进行检查，随后写原任务 queued、追加文本、再次 launch；`store.rs:869-879,3546-3559` 允许 completed/failed/cancelled/interrupted 等经显式 resume 返回 queued。重复请求若前一次已完成可以再产生一次新的模型完成调用；CLI 明确提示结果未知不得自动重复 resume（`cli.rs:3114-3119`）。如果完成的 A 已作为结果交给 B，随后 A 被 resume 改写输出，B 不回滚也不自动重算，因而当前无稳定版本化结果/图级重新计算语义。另一个具体风险：依赖 B 首次运行时将前置摘要追加 messages 并保存，之后 B 被显式 resume，`run_task` 每次再次追加前置摘要（`engine.rs:1940-1943,1970-1972`），可使历史摘要与新摘要并存；是否允许用户追加是产品语义问题，不称已实测数据损坏。
4. **外部副作用边界不能由任务 key 代替。** 新图提交在 `engine.rs:1678-1697` 先提交后 launch；同 key 同参数返回同一 Run，避免此入口重复启动，冲突检查在 Store 中完成。Host 在 Provider 发出后崩溃，恢复将 claimed 调用标为 unknown/interrupted（`network_authorization.rs:870-884`）；工具审批 claimed 亦为 unknown（`store.rs:2292-2295`），`engine.rs:2229-2237` 拒绝 unknown/claimed 审批的 resume。不能把 unknown 当作未发生或自动重试；显式 resume 若通过既有网络/预算/工具检查，会进入新一次 completion（`engine.rs:1973-1979,2004-2032`），因此可能产生新的远端请求或计费，不能称全局 exactly-once，也不能断言具体未知工具必然被重跑。已有 `engine.rs:6485` 子进程断点测试针对网络 claim 恢复不重发；此报告未执行该测试。
5. **Team 当前是批次任务和稳定成员映射，非完整持久 Team 管理。** `engine.rs:1584-1631` 为每个初始 spec 创建 Agent、任务和角色提示，图与同回合 ID 映射见 `:1641-1666`；系统提示明确无 Team membership tools（`:1594-1600`）。依赖完成时只把前置成员名/角色/模型/输出作为不可信数据传入下游（`:1940-1943`）；未见独立的 Team 结果汇总执行器或任意多回合 Team 新任务派发。`send_chat_turn` 只接受单 Agent、Chat、最新且完成的前一 turn，复用该 agent（`:1701-1705,1743-1773,1821-1850`）；Team 新 Turn 不能假借这一入口声称已实现。

## 已有可复用能力与直接证据

- `domain.rs:190-240,242-295`：Team 批次 1–32，名称唯一、前置存在且无自依赖/环，scope 冲突允许存在依赖顺序；Chat 限单节点，Plan 显式拒绝。提交路径验证配置、路由、权限，并把名称依赖映射为稳定 task ID（`engine.rs:1537-1563,1641-1666`）。
- `repository.rs:345-392,1406-1463`：持久 session、turn、agent ID/名称/角色、turn_task 身份状态和同 turn 依赖、请求 hash/key；旧 `Task` 持 spec、route、workspace、status、output、error、messages、usage（`domain.rs:318-330`），一次 bundle/事务持久新旧投影及生命周期事件。`store.rs:3584-3611` 从各任务状态计算 Turn 状态；这是状态汇总而非产物汇总。
- 调度容量：依赖等待后按实际 key 和 base URL 聚合路由限制，再获取全局网络槽（`engine.rs:1947-1972`），是当前 Host 内存中的信号量，并非持久跨 Host claim。权限/审批/预算等仍由既有 Host 契约执行，不能另建旁路。
- HTTP 有 runs、context、turn、task resume/cancel（`server.rs:443-445,474-482,1660-1675,1830-1845,1880-1895,1950-1998`）；CLI 有 session turns、task resume/cancel（`cli.rs:3572-3586,3714-3735`）。本次未把 CLI 说成独立完整 DAG 编排入口。
- 已有直接静态可见测试：`store.rs:5361` 同 turn 依赖与环，`tests/final_acceptance.rs:151` 无效依赖/缺投影回滚，`store.rs:5170` 重启中断，`engine.rs:6485` 网络 claim 强杀恢复；这些各自覆盖的边界不等同于完整多节点重启续调或本轮重跑结果。

## 建议的最小完整核心切片

先做现有 Engine/Store 的**持久 DAG 调度续接**，再以其为底座扩展 Team 任务创建、成员状态和结果汇总；不另造任务引擎，不扩 UI/MCP/Skills/高级 KeyPool。至少明确：图/节点身份和依赖在事务内稳定；持久就绪/运行/阻塞/完成事实与执行权原子判定，重启后区分未开始、已完成和可能已发出的 unknown，单一有效调度者及并发额度不能仅由内存状态推断；对失败、取消、恢复上游后下游如何阻塞/再准入、取消传播和用户可观察原因先定契约。仅对有可信证据未启动或可安全续接的节点调度；不可自动重放已领取/unknown Provider、工具或外部 worker，沿用现有收据、审批、预算与权限门禁。已完成结果需固定引用版本或明确禁止图消费后原位修改；消除摘要重复并按任务 ID 对应稳定输出。若 scope 恢复或成员结果有分歧应显式阻塞，不能默默重跑或覆盖。

改动尽量落在既有 `engine.rs` 调度入口与生命周期、`store.rs` 事务/恢复、`repository.rs` 图/必要持久状态及 `domain.rs` 契约，配合现有 HTTP/CLI 可观察接口；具体 schema、迁移和单写者/claim 方案由正式任务裁定。NEXT-21 外部 worker 未来需使用受信绑定和真实终止/回收证据，unknown 仍占槽或阻止再次派发；W3 设计不在本切片内。

验收建议以可见任务状态、事件、稳定结果和实际执行次数为证：线性链与扇入扇出、并发上限及 A 失败/取消后的后代和独立分支；A 完成后/节点提交前后/Provider 领取前后/工具执行边界强杀重启；同 key 相同与不同请求、重复恢复及并发恢复；重复 resume 与已消费结果稳定性；超时/未知执行不得重放，正确复用完成结果、无孤儿永久等待。真实系统测试需经理另批窗口；本报告未构建、运行测试或进程。
