# NEXT-21W3：持久回收证明独立设计

2026-11-01 · 员工 C / 6.1 Terra · 只读建议，待经理裁决；没有动态验证。本报告聚焦旧 Host 死亡后的 worker 退出证据，不重写 [第二十二轮事务与恢复设计](../第二十二轮/NEXT-21持久绑定与恢复独立设计审查.md)。

## 现状与必须修正的推断

- `NoManCode/rust-app/src/main.rs:94-125` 仅正常 main 针对数据目录 `instance.lock` 执行 `try_lock`，`--check` 跳过；`Store::open`（`store.rs:61`）可直接打开 SQLite，`Engine::new_recovered`（`engine.rs:783`）可调用 recovery。不能把正常 main 锁称为所有 Store/Engine 使用者的进程独占。`Store::recover`（`store.rs:2256`）把 queued/running 置 interrupted、审批 claimed 置 unknown；它无外部 worker 退出证明。未来写入/恢复/启动必须把同一个经规范身份绑定的实例锁令牌贯彻到所有能变更或启动的入口，直接 Store、多数据目录别名、符号链接/硬链接及多个 DB 指向同一状态也须裁决；未经锁令牌的管理/低层路径一律不可解封或启动。锁释放只说明锁不再由原持有者持有，不单独证明 kernel 中 worker 已退出。
- [W2 任务](../../../项目经理/任务/2026-11-01NEXT-21W2-B受控worker组件实施.md) 当前要求单进程、原子 Job-list、Job 仅 Host 持有、no breakaway、kill-on-last-close，尚无持久绑定；只是计划，不能视为已证实。匿名 Job 加 RAM 句柄在 Host 被杀后无法供新 Host 复核，W2 本身不应被称为已完成 W3 恢复。
- Microsoft [Life Cycle of an Object](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/life-cycle-of-an-object) 对临时对象的通用 Object Manager 语义说：句柄数归零后名称从命名空间移除，但仍可因引用存活；这是 driver 层一般说明。Microsoft [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) 对 Job 特别说明最后句柄关闭且关联进程终止后才销毁；设置 kill-on-close 时最后句柄关闭触发关联进程终止，之后销毁。两者共同说明 **OpenJobObject 找不到名称不能单独证明同一 Job 已空、进程全部退出**；Job 特定竞态和本机表现仍须实测，不能把通用文档当已执行的 Job 实验。
- [CreateJobObjectW](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-createjobobjectw) 可以打开同名已有 Job 并报告 `ERROR_ALREADY_EXISTS`，名字与其他对象类型共享命名空间；[OpenJobObjectW](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-openjobobjectw) 失败要分别读取错误。随机名字不等于对象认证：预创建/名称冲突、错误 session 或 namespace、DACL 拒绝、命名空间或重启变化均不能等价为空。新 Host 如果提前持有 Job handle，还会阻止 old Host 的关闭成为最后关闭，使 kill-on-close 不按预期触发。Job handle 非继承、不泄漏或外部复制是前提，非既有事实。
- [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) 中等待 Job 的 signaled 状态只描述 end-of-job time limit 触发时，不能把一般 `WaitForSingleObject(job)` 当作任意退出/清空证明。持有可信 Job 句柄时须通过 [QueryInformationJobObject](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-queryinformationjobobject) 的 `JOBOBJECT_BASIC_ACCOUNTING_INFORMATION.ActiveProcesses` 配合其他条件核查，不把通知丢失或超时当 0。Microsoft [TerminateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-terminateprocess) 明示对其他进程异步，返回成功不等于结束，须等待**该进程真实 handle**信号。

## 最小执行与持久契约

为避免以 PID 推测身份，推荐优先**受信原 Host 已拿到的 worker process handle 证明**；一个 DB 标识不是可跨崩溃续用的内核 handle。只支持 W2 已规划的单 worker、active-process limit=1、无后代/不允许 breakaway，且 atomic Job-list 纳入、没有子进程逃逸/Job handle 泄漏的全程机制需要实际验证。若这些条件未建立，单个 worker 退出不能证明域空。

每次请求持久登记最少：不可复用 request ID、task/binding generation、owner epoch、机器/可信启动代、状态及单调 state version、受控工件身份/digest；启动后原 Host 自己从真实 process handle 得到 PID 与 creation time，再在 **resume 之前**以 IMMEDIATE/CAS 写入这些观察及 `registered_suspended`。持久化前不能将结果交给业务或释放 guard。PID 和 creation time 用于识别候选、拒绝明显不符及审计，不是凭据或退出证明。另有创建阶段 `intent` → `creating` → `registered_suspended` → `resumed` → `completed/cleanup_unknown`，以及原子结果采纳 gate、撤销/清理状态与请求级幂等回执；具体物理表可合并，但状态不变量不可省。`creating` 必须在发起 OS 创建前提交；建立的最小字段记录是否进入启动边界。不是创建成功后再补登记假装无窗口。

一把规范化的数据域独占锁（需覆盖全部 Host/Store 启动变更路径）与 DB owner epoch/gate 共同使用，独立 SQLite `IMMEDIATE` 为所有授权操作线性化。不能以同一数据库两个不同 `instance.lock` 路径、`--check` 或单个进程 mutex 绕过。管理员/本地恶意系统级对手、受信二进制/DB/OS 被篡改不在此证据模型；如需抵抗同用户恶意进程，要单独规定对象 DACL、身份与数据库认证，单靠随机名字不足。

### 崩溃停点与可执行恢复

1. **intent 已持久、未曾进入创建。** 仅当状态机在授权唯一入口上能严格证明创建系统调用从未被发出（同一事务提交明确 `creating` 之前，且不允许旁路）才可直接标记未启动。不要从没有 PID 推导未启动。
2. **creating 已提交但 process 身份未持久。** 在 Job 创建、CreateProcess 正在进行、创建成功到 PID 登记提交之间任一点失去 Host，都是 `cleanup_unknown`。即使启动参数规定悬挂、active=1、无 breakaway，也可能已有一个未登记进程或系统调用未完成；PID 枚举、job 名称缺失、owner 不存在、超时及读取旧 result 不足以关 unknown。先持久封门、占有范围内唯一槽并阻止再次创建；只读观察不解封。此窗口是当前设计的实质缺口，不能承诺同一 OS boot 内自动消除。
3. **registered_suspended/resumed 已提交。** 恢复者先取得同一数据域排他锁、验证当前 owner epoch 已被围栏且原 Host 的所有创建提交系统调用已结算；若仅凭进程死亡不能证明内核侧不存在迟到创建，则维持 unknown。可在不打开 Job 的前提下，对已登记 PID 通过 `OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION)` 获取真实 process handle，核其 `GetProcessId`、`GetProcessTimes` 的创建时间及当前记录代；权限拒绝、进程已消失、同 PID 身份冲突、查询失败全为待证，不以 PID/时间本身解除。若获取到真实可等待 handle，则对该对象执行等待；只有 `WAIT_OBJECT_0` 才形成该对象退出证据。可在原 Host 活着时预取 process handle，但不能对外采用结果或启动替代 worker；拿 process handle 不等于拿 Job handle，仍需验证不会让 Job 的 last-close 被延后。原始 worker 是否允许终止、process 句柄查询 ACL 能否被恢复者打开以及真实 wait 效果均属待测。`WAIT_TIMEOUT`、`WAIT_FAILED`、访问拒绝都不通过。
4. **验证与提交。** 在拥有锁令牌且确认门已关、无迟到 create/submit/resume、无后代条件成立后，新 Host 的受信恢复代码内部记录 worker 真实进程对象信号和 API 成功结果；对 DB 以 `(operation_id, request_id, owner_epoch, binding_generation, state_version, gate_closed)` 比较并交换，将 `cleanup_unknown → completed`，写不可变恢复回执。请求可认证且带幂等 key；相同 key+摘要返回原回执，冲突拒绝。证据只能来自受信本机程序直接持有的 OS handle 或下述可信重启见证；不能接受用户提供 `true`、PID 或路径。若状态/owner/epoch 变化则重新核验，不能将旧信号用于新绑定。已 claim 文件、网络、Shell 或 Provider 仍由原先的审批与 unknown 规则管理，不能因 worker 退出而重放。

进程可能早已退出、process 对象被释放，导致后来的 `OpenProcess` 不能取得可等待 handle。`GetProcessId` 官方只承诺进程终止之前 PID 在系统唯一，不保证 PID/时间组成不可伪造的永久身份；[OpenProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocess) 的错误也不能一概解释为已安全退出。若缺 handle，按 unknown 处理；这是可用性限制，不用空枚举掩盖。任何额外 Job 恢复操作必须先证明所打开的是同一安全对象并确保原 Host 已退出；未完成这些前置不能打开并当成 kill 完成证据。

## 处理无句柄或创建窗口：受控整机冷重启边界

这是不引入持有 Job 的常驻 supervisor 的可恢复路径，**需要额外的可信重启见证能力及其验收，当前代码/官方资料没有证明它已经存在**：

1. 认证管理员在可信恢复界面请求受控冷重启，事务保存新鲜一次性 challenge、旧 machine identity、旧可信 boot incarnation、所有相关 unknown 的 request/epoch/版本摘要和关门状态；不因用户口头声明完成而解除。隔离状态跨重启持久保存，恢复前所有 worker 准入保持关闭。
2. 使用能为**同一物理机或受控虚拟机**提供不可伪造、可校验见证的独立可信平台控制面，执行完整断电/冷启动或等价的明确全重置，出具签名收据：机器身份、challenge、旧/新 boot incarnation、全重置完成及防重放序号。它是按需重启与证明来源，不持有旧 Job，也不是 worker 常驻 supervisor。恢复者校验证书/授权、challenge、同机、旧新代、序号、新 OS 启动后本机身份和数据库门；收据缺失、伪造、重放、机器换置或回滚均拒绝。见证格式、硬件/虚拟平台可信根、API 以及与本机身份的绑定须作为未来可交付实现与动态验收，不得声称现有普通 Windows 主机已具备。
3. 所有在旧启动代内的进程/未完成创建只有在该**经证明的完整重置**下才可判定不再存续；通过后逐条以 request/generation/version CAS 记录证明及完成，保留其他业务 unknown。对不支持可信平台重置见证的普通本机，此路径暂不可自动通过；可通过受控迁移到具备该能力的受信环境或建立并验收等价可信见证后恢复，期间继续封门，不设置超时自动放行。

[Microsoft 冷启动与快速启动说明](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/distinguishing-fast-startup-from-wake-from-hibernation) 区分冷启动与加载休眠映像；睡眠、休眠、快速启动及仅仅服务重启不能作为本方案全重置证明。[GetTickCount64](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount64) 是系统启动以来的时间、不是不可复用且防篡改的 boot 身份；WMI LastBootUpTime、PID 创建时间和一般日志时间单独也不足以认证完整重置。平台见证缺失时本方案明确阻塞，而不是把永久 unknown 宣称为已交付恢复能力；W3 若不能提供可用见证，应限制发布承诺并保留具体补齐条件。

## 必须的反例与验收

| 停点/竞争 | 必须结果 |
| --- | --- |
| intent 前后、creating 提交到内核调用返回各点 Host 真杀 | 仅证明未发创建者可完成；其余 unknown，无再次启动；迟到成功进程不得执行或逃逸 |
| 原 Host 活着、预取 process 与 Job handle | process 观察者不影响结束的假设需实测；提前 Job handle 会延迟 last-close，应被协议阻止或标明需显式终止；无默认全局观察句柄 |
| 真实 PID 复用、同名伪造、其他 object 类型、拒绝访问 | 不以 PID/creation time、OpenJob not found、ERROR_ACCESS_DENIED 或冲突宣称退出；错误原因与关联代分开 |
| registered 后死亡，worker 退出前后分别重启 | 取得同代真实 process handle 并等待 signal，且创建已结算、单 worker/no breakaway 才可 CAS；对象消失或权限不足保持 unknown |
| 两 Host、DB alias、旧结果和重试交错 | 锁令牌/DB epoch fence 保证至多一授权者；旧结果及 ABA 不能采纳；管理请求相同 key 重放与不同摘要冲突正确 |
| Job last-close、process signaled 与 handle 清理的真实顺序 | 按实际 OS handle 观察及 Job accounting 验证，不以 Job wait 普通信号、终止 API 返回或 timeout 代替最终证据 |
| 冷重启、休眠、快速启动、假/重放/错机见证、回滚 | 只有可认证、绑定原请求代的完整重置见证能释放；其余 unknown；无可信重置平台的限制可见 |
| 已 claim 的工具/Provider 及未知清理 | 重启只解除 worker 证据，不改变或重放业务外部效果；故障不能产生双执行 |

## 固定基线与交付边界

检查时主仓 HEAD `8128f5c135ba0ae96ba8d2be40710e6fdc81d1a4`，产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58`、Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`；index/unmerged 空，人员继承改动保留。只读了 B 已签 W2 任务及现有代码；官方文档页面返回有正文。未修改产品、运行 Host/进程实验、构建或测试。建议经理先决定 W2 保持组件范围、W3 将持久 process 身份/状态接口和可认证冷重置见证单独派发；在对应证据和实验完成前，保持 unknown。
