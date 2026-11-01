# NEXT-21W2：强杀与句柄继承夹具预审

2026-11-01 · 员工 C / 6.1 Terra · 第三十轮只读。主仓固定 `85fb632d28cd5f161ea1d2aa85558fc7f4edb023` / 产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58` / Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`；W2 仅用 Git blob `ba4f72cf70c75d25880a63fdb50538204c37d762` / 产品 `5e7eab49deeeb1de1f979e2c783e89c99bea46bd` / Rust `1245c1592ff237c8cf5b978fc921b0f1f43e31e5`。没有以 B 在途树作为候选，没有构建或运行。

## 固定代码与测试边界

W2 `src/wasm_worker_process.rs:138-160` 的可信工件及 service 构造是 crate-private；`src/lib.rs` 将 `wasm_worker_process_tests` 作为库内部测试，故可在相同 lib 测试二进制内使用该构造。`:1419-1424` 设置 Job-list 和仅三条 stdio handle-list，`:1448-1522` 有 before-create、创建挂起、Job/映像核对、resume 和记录阶段；`:1195-1200` 的 `stdin_pending` 是写操作 poll 后仍 pending 的观测。当前文件阶段日志及同进程 `test_observer` 不是跨 Host 杀死后的外部同步证明，需要新增明确屏障和外部观察；其 Job 关闭、原子归属和不继承仍须真实验证。

下面的 Host helper 是**运行真实 W2 library 代码的测试进程**，不是当前正式 `peachsh.exe`/Engine 接线；其通过不替代正式入口后的端到端故事，也不说明 native 文件/网络隔离。真实 W1 worker 的正常业务前后决策已经由 W01 覆盖；W05 可对挂起用固定真实 W1，对需要进程启动或不读 stdin 的握手使用限定 fault-probe，报告必须逐例标明 worker 身份，不能把 fault-probe 误称真实 W1 业务执行。

## W05 三方夹具

1. **外部父观察者**为同一已编译 lib test 可执行文件中单线程父测试。它使用 `std::env::current_exe()` 启动自身的另一个 `#[cfg(all(test,windows))]` helper 测试函数，以 `--exact`、单线程和测试专用角色/nonce 进入子分支；正常 suite 没有 helper 标记时该函数不产生任务。**不递归调用 Cargo**，也不从生产 HTTP/CLI/插件提供任何可执行路径或 hash。最终 worker/probe 都取由外层 runner 在全部会改 target 的构建后固定复制并核源/副摘要的测试快照，Host helper 只从测试专用配置构造 `TrustedWorker`。
2. **Host helper**是真实执行 W2 `WorkerService::call`、创建并唯一持有 W2 Job 的进程。父测试用受信、单次、有界的控制通道与其通信，消息绑定测试 nonce、helper PID、场景和递增阶段；可用测试专用命名事件/匿名管道，控制句柄仅给 helper、进入 W2 worker 之前设非继承，绝不在 worker 白名单里加入 Job 或控制句柄。控制对象 DACL/随机名/父子端身份和时限明确；测试日志只做保全，不用文件存在或固定 sleep 充当唯一就绪握手。helper 按 `cfg(test)` 停在真实 W2 调用点，不改变生产执行路径、Job/资源限制或取消语义。
3. **worker 观察**：helper 在 `CreateProcessW` 成功并拿到实际 `PROCESS_INFORMATION.hProcess`、仍处于挂起时发送 `(pid, creation FILETIME, nonce, stage)`。父方 `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)`，对获得的 handle 使用 `GetProcessId/GetProcessTimes` 核同一 PID 与创建时间，并验证该 handle 尚未 signaled，回复已捕获后 helper 才能继续。整个后续等待始终使用此**同一已持有的真实 worker handle**，不是再根据 PID 打开；捕获期间 worker 保持挂起，防 PID 复用。父方另持用于 kill/wait 的 helper process handle，**不打开、复制或继承 Job handle**。若系统/权限不允许打开 worker，场景不成立，不拿文本 PID 顶替。
4. helper 测试专用阶段可增加“真实 Job 已应用 active=1、kill-on-close、无 breakaway、Job handle 不可继承、worker 实际属于该 Job”的查询及精确结果，记录在 helper；外部父方不取得 Job handle。确认父测试进程不拥有该 Job 的额外 handle；如进程处于测试运行器外层 Job，记录嵌套环境，确保外层 Job 在观察期间一直保持、没有外部杀目标动作，避免把外部终止误归因给 W2。

Microsoft [Handle Inheritance](https://learn.microsoft.com/en-us/windows/win32/procthread/inheritance) 说明继承的句柄指向同一对象且数值相同；[TerminateProcess](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-terminateprocess) 明示对其他进程的终止是异步，须等待真实 process handle 信号。正常 Host 死亡后其 Stopper 线程也不存在，不能以 Stopper 发起 terminate、回执或协作 cleanup 代替 Job 最后句柄关闭的证据。

### 就绪、杀死与断言

各场景通过独立试例和新鲜 nonce/快照、仅一条活动调用，记录创建点、真实 API 结果、前后时间、PID/创建时间、控制消息及相关 hash。W2 固定 5 秒调用限额，屏障、捕获、杀死动作必须在原绝对 deadline 之前完成并保留足够裕度；确认 gate 未因取消/到期关闭、Stopper 未发起终止、worker handle 为 WAIT_TIMEOUT，再由父方对 **helper 的真实 process handle** 调 `TerminateProcess`（或 `Child::kill` 映射等价操作），随后等待 helper handle `WAIT_OBJECT_0`，再有限期等待早已捕获的 worker handle `WAIT_OBJECT_0`。记录 API 返回和实际观察时间；仅 kill 请求成功、PID 消失、helper 退出或 timeout 均不是 worker 已退出。若计时/Stopper 先到、控制失联或顺序不能证明，则该例无效，不能算强杀通过。

| 场景 | 准确停点与必须握手 | 完成条件/范围 |
| --- | --- | --- |
| 创建前 | 在 `:1448` 与实际 `CreateProcessW` 调用之间加 test-only 阻塞；helper 发送唯一阶段并等待父 ack，保证唯一创建线程在**调用前**且没有其他发起者 | 杀 helper 后等待 helper signal；由真实屏障位于调用前、唯一创建路径和 Host 终止证明没有此次 worker 创建。不能用“没有发布 PID”或进程枚举推断；记录此例无 worker handle、无 worker signal |
| 已创建仍挂起 | `:1497` 之后、`ResumeThread` 之前阻塞；helper核原 process handle/Job 约束，将 PID/creation time 回父，父先打开并验证同一未 signaled worker handle | 父杀 helper、等待 helper 及同一 worker handle signal。不得先恢复 worker；不必观察 worker 自身握手。可用真实 W1 固定工件 |
| 已 resume 且 worker 实际运行 | 先在挂起点让父取得真实 worker handle；继续至 `ResumeThread` 成功后，以测试 probe 的受控 started 握手确认 worker 真执行，再暂停 Host 于受控点；父确认 gate/deadline 和 worker仍存活 | 同样杀 helper 并等两个真实信号；resume 返回只证明发出恢复，不单凭它声称用户代码已运行。可用已固定 fault probe 的 `started` 受控阶段，报告明确是 probe |
| 真 I/O pending | 先捕获 worker handle；以固定 `no-read` probe 保持 stdin 不读，发送接近 8MiB 请求，在真实 write 已提交且 poll/event 确认仍 pending 的点阻塞 Host；probe started/no-read 和 supervisor pending 各自握手 | 父验证 gate 未到截止且 worker 仍未 signal，再杀 helper；等待同一 worker signal。保全已提交 I/O/缓冲的旧 Host 状态与结果；此例测 Host 意外死亡后的进程终止，不把已死进程内 OVERLAPPED 收割日志伪造成存在 |

上表末两例可复用 W02/W03 已有固定 basename probe，但必须单独快照和阶段证据；`no-read` 不应先等待完整 stdin 才选择模式。若要另加真实 W1 的长运行态作为补充，须有真实 worker 已执行的可验证握手且不改变 W1 生产入口，不能以任意睡眠/CPU 时间假装证明该停点；做不到时明确未覆盖。

### 故障与有界清理

- helper 未就绪或阶段握手超时：先通过有界通道请求仍存活的 helper 按 W2 原回收流程取消，确认具体 process/Job/I/O 完成；若不响应才按授权使用持有的 helper handle 强杀并等待。若没有取得 worker 观察 handle，即使 helper 死亡也记 `cleanup_unknown`，不把无 PID/`OpenProcess` not-found 当完成；停止后续试例并保存阶段与错误。
- worker `OpenProcess` 失败或身份不匹配：在 helper 仍掌握原 worker process/Job handle 时请求受控取消/终止、等待真实 signal 和 Job zero；任何证明缺失即 unknown，不能把父测试失败算强杀通过。若 helper 已死亡、父只持同步句柄且 worker未 signal，同样保持 unknown，不能按名字或可能复用的 PID 杀进程。
- `TerminateProcess(helper)` 失败、helper/worker wait 超时、通道/观察者本身故障：逐项保留 Win32 API 返回/错误及持有句柄的状态，不称成功。父方的 RAII 测试 guard 应在 panic/unwind 上关闭控制通道、有限等待，并对**已持有的 helper process handle**做定向终止与等待；在父进程真正被外部强杀时 RAII 不执行，故不把 guard 作为全部失败兜底。测试超时后若无 worker 退出证明则 unknown、停止窗口并交经理处理；后续如需独立人工清理必须单独受控并核原对象，不按映像名全局清理，也不通过关闭 Cargo 声称回收。不能通过测试守卫持有额外 Job 来“修复” W05，否则改变最后句柄条件。

## W06 非白名单继承与 Job 反例

**非白名单哨兵：**在 Host helper 的真正 W2 调用前创建一个**匿名**、最初 nonsignaled 的 `CreateEventW`，`SECURITY_ATTRIBUTES.bInheritHandle=TRUE`，用 `GetHandleInformation` 核确实可继承。将该数值和本例 nonce 经仅测试专用 fault-probe 请求中的有界数据传给 probe；正常 W2 的 handle-list 仍仅 stdin/stdout/stderr。probe 只对该数值尝试 `SetEvent`，返回 Win32 原始结果；Host 在其原始 sentinel handle 上等待/查询同一对象。若 sentinel 被 signal 则证实泄漏；若 `SetEvent` 失败、且原 sentinel 未 signal，可证明该尝试未触及 sentinel。若 `SetEvent` 成功但 sentinel 未 signal，则该数值可能是 worker 新分配的其他 event，必须记录**数值复用**，不能把成功误报继承或把任意句柄数值比较当对象身份。

为消除碰巧复用的不确定，helper 可用自己已持的 worker process handle（需 `PROCESS_DUP_HANDLE`，CreateProcess 原始 handle 的实际权限需核）将 worker 中该数值通过 `DuplicateHandle` 复制回 **helper 自己**，再以 `CompareObjectHandles` 与原 sentinel 比较，关闭临时副本；不同对象或该数值确证无效才是不继承该 sentinel 的证据。无法复制/比较、权限不足、目标过早退出则记未判定；无其他同进程线程关闭/重用相应句柄的前提要靠受控停点保证。该 API 的 [CompareObjectHandles](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-compareobjecthandles) 官方最低客户端 Windows 10；[DuplicateHandle](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle) 说明复制后是同一对象。W05 外部父观察者仍不持 Job，也无需持该复制的对象；这一步在持有原对象的测试 Host 内完成。

正控另用**仅测试 launcher** 显式将 sentinel 加入单独的 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`，确保 worker 能改变该唯一 sentinel、比较结果相同；该正控不修改生产 W2 的三句柄列表、不用作实际隔离结果。负控要求 worker 仍能使用自己三个 stdio，且真实 W2 运行行为保持：故障 probe 是测试专用，正常 W1 业务接口不接收任意句柄或测试参数。

**Job 句柄未泄漏：**在 helper 内真实查询 Job `HANDLE_FLAG_INHERIT` 为关闭；test-only 停点传真实 Job 数值供受控 probe 尝试使用，再由 helper 从 worker 复制对应数值并以 `CompareObjectHandles` 与 helper 手中原 Job 比较；若数值已被复用为其他对象，必须按对象比较而非数字判断。不存在/不同对象是本次数值不泄漏的证据，查询/复制异常不等于无泄漏。若真的复制到了 Job，立即在 helper 内关闭临时副本；W05 不能同时运行此探测，避免该额外 handle 改变 last-close。结合三句柄白名单、无额外复制的代码与继承正控，不夸大为全系统任意句柄不存在的证明。

**active=1 / breakaway：**先在挂起时由 helper 查询并记录实际 `ActiveProcessLimit=1`、未置 `BREAKAWAY_OK`/`SILENT_BREAKAWAY_OK`；再让受控 fault probe 分别尝试普通子进程和显式 `CREATE_BREAKAWAY_FROM_JOB`，记录真实 API 返回/错误，不以配置查询冒充实际尝试。若意外创建成功，必须以 `CREATE_SUSPENDED` 确保子进程未执行，受控 helper/fixture 取得该子进程真实 handle、定向终止并等待，任何无法取到/回收均 unknown 并停止；不以 PID 名称清理。两项同时限制时，单个失败不能唯一归因 active=1 还是禁止 breakaway；需要**独立测试控制**（测试 Job 放宽其中一项、另一项保持）证明敏感性，控制 Job 不是 W2 成功结果，不能把测试专用放宽部署。Microsoft [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects) 描述默认子进程与 breakaway 规则；嵌套 Job、环境策略或 OS 差异要分别记录，不把任何失败码默认归因本 Job。

## 最小路径、验收与未覆盖

建议 B 后续只扩展已授权的 `src/wasm_worker_process.rs`（`cfg(all(test,windows))` 有界停点与不暴露 Job 的观测）、`src/wasm_worker_process_tests.rs`（父/`--exact` helper 与 W05/W06 断言）、`examples/wasm_worker_process_probe.rs`（固定 basename 的测试模式）和必要 `tests/support/wasm_worker_process/`（受控快照/清理 runner）。这些构造/控制只能用于测试目标；若发现确需其他路径或 Cargo target，先列理由交经理，不在本报告自行授权。现有 `windows-sys 0.59` 的 `Win32_Foundation`、`Win32_Security`、`Win32_System_Threading`、`Win32_System_JobObjects`、`Win32_System_Pipes`、`Win32_System_IO` 等 feature 已在固定 Cargo.toml 声明；本机固定 bindings 中 `DuplicateHandle/CompareObjectHandles` 在 Foundation、`GetProcessTimes/CreateEventW/SetEvent` 在 Threading，初步不需新增 feature，编译需后续验证。若 control pipe 或其他 API 实际缺 feature，逐项列最小新增并经经理裁决。

通过标准是每一停点都有目标阶段握手、同一真实 handle、Host kill/wait 和相应 worker signal，或创建前的明确无调用证明；异常必须明确 unknown，所有日志与工件 hash 保存。W06 需 sentinel 正/负控、Job 对象身份检查及分离 active/breakaway 的敏感性控制；仅 `is_err`、线程睡眠、同数字句柄、Host 协作 cleanup、Job 配置或 PID 文本均不足。

工件替换/写映射及非 Windows 支持仍按既有 W06 后续范围，本文不重复；也不扩 W04 资源/取消矩阵、W3 持久恢复或生产 CLI 功能。本轮没有动态证据、产品改动或新的平台实现；待经理转 B 细化并安排验证窗口。
