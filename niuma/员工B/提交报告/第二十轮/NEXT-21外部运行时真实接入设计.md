# NEXT-21：WASM 与外部进程真实接入方案

2026-11-01，员工 B；供经理冻结实施契约，并非已实现或已验证的安全保证。依据 [正式任务](../../../项目经理/任务/2026-11-01NEXT-21-B外部运行时接入设计.md) 与当前 main 代码。基线 HEAD `d6480e4b3daf9b525b6a693ac8da8edc24ee0dbe`，NEXT-20 merge `a5d429ec8ba5dec55305f25a8ab6f33bec0fa97b` 为祖先；NoManCode tree `897cc8510c0c56118df04c2c1dbcea9c999e539f`，Rust tree `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`。产品无 diff；仅本人报告/身份可写，未构建、运行 Host 或测试，manager 管理共享 index。

## 1. 裁决与不可降级边界

首个对外能力选 `agent.strategy@1.0.0`：外部策略只看工具类别、轮次和完成类型，可选子集、逐项 `apply_host_policy`/`deny`、完成或停止；它不接收消息、用户文本、路径、参数、凭据或数据库句柄。最终项目绑定、scope、审批、网络/预算及工具结果事务仍在 Host。它不是通用第三方工具实现，但覆盖真实 catalog、typed binding、工件校验、Host 生命周期、持久选择、任务调用、取消及失败恢复；其他 interface 后续沿同一 adapter 扩展，须另定权限与事务契约，不把本轮的纯策略权限泛化。

两个互相独立的执行形态：`WasmStrategy` 是无 imports 的 JSON 纯计算；`ProcessStrategy` 是 Windows LPAC 中只收发协议的受限计算。普通同用户进程即便清理环境、套 Job，仍可能读取 Key/SQLite/文件或自行联网、创建进程，**不满足项目书，禁止作为 fallback**。LPAC/ACL/能力限制能否在目标系统实现所需边界，本方案未实测；先完成第 7 节先决试验，失败或不能证明时 `isolation_unavailable`、不启动 process、不激活其 effect。WASM 路径可独立交付；只有隔离试验与生产真实链两者完成，才能称 process 已交付。

## 2. 身份、装配与旧版本兼容

- 区分 `management_domain`（例如 `external.agent:<plugin_id>`，用于单个插件的 desired/CAS/锁）、真实 `plugin_id`（由 Host 验证的 manifest id，不可 `builtin.*`）、`artifact_id`（完整规范化工件包 SHA-256）、manifest `version`、`InterfaceKey { service, "agent.strategy", 1.0.0 }`、持久 `desired_revision`、进程内 `generation`，各有不同用途。摘要证明字节一致，不证明发布者可信；安装和启用需用户明确授权具体摘要及隔离模式。原 Agent 的 domain 固定为 `builtin.chat-agent`，不复用该 id 保存第三方状态。
- v1 保持旧 `/api/plugins` 两项、`/api/runtime/components` 两项及其字段/含义，`/api/host/composition` v1 的 `chat-agent.provider_id=builtin.chat-agent` 继续描述默认内置绑定；不把第三方伪装成内置或偷偷增加闭集字段。默认/未配置项目继续走 builtin。新增受原 token、Host、Origin、跨站检查的 `/api/external-agents` 安装/查询/启停、`/api/projects/{id}/agent-strategy` 查询/选择、`/api/tasks/{id}/agent-strategy` 查询实际 task binding；新版 CLI 可显式访问。项目选择外部策略不改变 v1 的 **Host 默认**绑定；任务查询必须显示实际第三方 id/digest、snapshot revision 和当前状态。旧客户端继续解析旧 DTO；不能依据 v1 断言所有项目均用 builtin。
- 实际路径：用户在正常 main Host 上安装并启用工件、对项目明确选择；原 `start_in_project`/`send_chat_turn` 在原事务中读取项目策略并将选择固化到每个新 task，`Engine::run_task` 依据 task binding 从外部对应的 AgentRuntime 获取真实 lease，再进入现有 Before/After 业务循环。`/api/runs` 的 selected-project 路径同样走此逻辑。无需测试注入或绕过 normal main；新接口不可绕过既有 project/config/CAS。无第三方配置或旧 task 无绑定时只解释为原内置语义，不尝试磁盘发现或自动替代。
- 插件 Host 的真实实例不再限于 `BuiltinPlugin`。建议 `RuntimeAdapter` 按 `RuntimeKind` 构造 `Builtin | Wasm | Process` 受控实例，`prepare` 只创建无权限执行句柄，Host 检查 manifest、准确 `InterfaceKey` 与 payload 类型后，自己以实际 plugin id 注册唯一 `Arc<dyn AgentStrategy>` effect。外部字节不能调用 `PluginContext`/自注册任意 effect。每个外部 plugin/代有独立 `PluginHost`/catalog，消费者引用其明确 provider，避免多个 provider 对同一个 key 造成歧义；内置域保留。拒绝缺依赖、身份/类型/版本不匹配；本次部分激活的 effect 和 worker 逆序撤销，不动旧活跃代。
- 不将不可信调用放在 `PluginHost` 锁或总组合锁内。短锁 reserve/取快照；耗时校验、进程启动、等待及析构在锁外；Host 在已验证句柄就绪后短锁提交/登记 effect；如果此间 revision 改变则丢弃候选并关闭句柄。生命周期数据结构需显式 `preparing/active/draining/failed` 防止 ABA；跨域失败不锁住其他域。

## 3. 确切 wire、typed 边界与 Host 准入

旧 Agent 域的持久 enabled 是所有 Agent 的**总准入闸门**，包含项目选择的外部策略；`builtin.chat-agent` disabled 时即使外部插件自身 enabled，也不得取得新 lease，v2 观察标为 `blocked_master`。旧接口仍真实显示内置域的 enabled/运行态，额外的总闸门语义在新接口和文档明确说明。外部 task 的 lease 同时持有原 Agent 域的 `AgentMasterGuard` 和外部域 guard：master guard 使用**同一** AgentRuntime 的 admission/transition 状态与 `in_flight` 计数，不获取、不冒充 builtin strategy；外部 task 从策略取得直至释放整个任务 lease 都持有 master guard。旧 disable 关闭 master admission，新 lease 全阻断，draining 与 in_flight 包含外部 task，直至 guard 释放才可宣称 drained。已准入 lease 按原 Agent drain 语义完成，除非用户另行对外部插件明确 revoke。绝不能只读取旧 enabled 后让外部 lease 脱离旧生命周期。

内部建议：`StrategyIdentity { plugin_id, artifact_id, interface: InterfaceKey, desired_revision, generation }`；`trait AgentStrategy { fn decide<'a>(&'a self, request: StrategyRequest, cancel: CancellationToken) -> Pin<Box<dyn Future<Output=Result<StrategyAction, AgentError>> + Send + 'a>>; }`。`AgentLease` 持不可变身份和受控 adapter，内置策略实现同一异步边界；`Engine` 的两处调用 await，并在取消分支与 worker 确认回收后结束。原 AgentLease 对阶段、索引排序/不重复、票数和 Stop 的验证保留并对所有来源执行。

跨 runtime JSON v1 使用 `serde` 闭集（重复、未知、缺失、null、类型错均拒绝；完整 UTF-8、单一 JSON 值，禁止尾随内容）：

```json
{"v":1,"id":"request-opaque","kind":"before","round":0,"max_rounds":8,"classes":["read","write","command"]}
{"v":1,"id":"request-opaque","kind":"use_tools","indices":[0,1]}
{"v":1,"id":"request-opaque","kind":"after","round":0,"max_rounds":8,"outcome":{"kind":"tool_group","classes":["write","command"]}}
{"v":1,"id":"request-opaque","kind":"tool_votes","votes":["apply_host_policy","deny"]}
{"v":1,"id":"request-opaque","kind":"after","round":1,"max_rounds":8,"outcome":{"kind":"no_tool_calls"}}
{"v":1,"id":"request-opaque","kind":"finish"}
```

每种输入也可只收到 `{"v":1,"id":"request-opaque","kind":"stop"}`。`classes` 枚举严格为 `read/list/search/write/command/other`；`0 <= round < max_rounds`、`max_rounds` 不超过 task 已验证值，classes 至多 256 项，`indices` 严格升序、唯一且小于当次长度；tool_votes 恰好对应工具调用数量。Before 只接受 use_tools/stop，after no_tool_calls 只接受 finish/stop，after tool_group 只接受 tool_votes/stop。错误、缺应答、无效次序、重复/未知 id 均 fail closed，不能转换为 ApplyHostPolicy 或 builtin fallback。`id` 仅随机关联本次请求，不是授权、身份或可重放凭据；纯决策失败不构造工具执行结果。该 input 刻意不传原接口的 `messages`，内置策略仍可保持原行为；未来有信息扩展须另行隐私/权限契约。

ToolCall 使用现有 `ToolCallApplication`：插件 `deny` 走已有 `Origin::AgentDenied` 和持久结果，`apply_host_policy` 仍先验证 advertised 工具、ProjectService、ApprovalService、网络告知及原 lease/claim；插件既不能发起原始工具，也不能产生授权或把被拒绝改成批准。实际示例：Before 将 `[read,write,command]` 过滤为 `[read,write]`；另一个例子 After 对 write 投 deny，确保真实模型调用被持久记为 AgentDenied 且无文件副作用。

## 4. 工件、安装和供给链

工件包 `manifest.json` 为严格闭集：`format_version=1`、外部 `plugin_id`、规范三段式 `plugin_version`、`runtime=wasm|process`、`interface={kind:"service",name:"agent.strategy",version:"1.0.0"}`、`entry`、`files:[{path,size,sha256}]`、`abi`、`limits`。按 UTF-8 字节序排序、固定 JSON canonical encoding、domain tag `nhc-agent-artifact-v1\0` 后 SHA-256；`artifact_id` 覆盖 manifest 规范表示（不含自身摘要）及所有相对路径、大小、文件摘要。拒绝重复规范化路径、绝对路径、`..`、Windows 大小写冲突、链接/reparse/hardlink 逃逸、未知文件、目录/总量上限与可执行外依赖；对 process 包将所有非系统 DLL 等依赖列入且校验，禁止通过 PATH/工作目录搜索外部代码。由 Host 把包复制到数据目录内内容寻址 staging、fsync/原子发布；不覆盖既有摘要目录。上限建议 manifest 64 KiB、单文件 32 MiB、总包 64 MiB、文件 64；WASM 模块沿现有 8 MiB 上限。

Host 每次构造新运行代用防 reparse 的句柄打开并复核目录/文件身份、全部 digest，执行文件以禁止写/删除共享的句柄持有直至安全进程启动；核对已启动映像路径/文件身份并在释放句柄前验证，失败 kill+wait。必须用 Windows 支持的安全打开方式做 TOCTOU 测试；如果不能在该文件系统保证此链，不启用 process。运行时不从用户配置直接执行可替换路径。`plugin_id/version` 只是声明而非真实性证明，许可绑定 artifact 摘要；升级、新包不同摘要必须重新确认，不自动信任同名同版本。安装阶段磁盘写与 SQLite 提交崩溃处理：staging 从不视为已安装；落盘后 DB 未提交的内容仅孤儿，可人工/未来单独受控回收，本轮不删除历史现场；已提交但缺损的安装为 unavailable，不伪成功。

## 5. WASM 和 process 执行、取消与限制

WASM 沿用现有 `peachsh.wasm.v1` `memory/alloc/run_json` ABI，但 adapter 必须复用无 imports、module 大小、memory/table、fuel 和指针/长度检查，不直接用现有开放的通用 HTTP `run_wasm` 旁路。每次决策新建 store/instance，生命周期不跨调用，禁止 module imports/WASI。初始化/start、alloc 和 run_json 都计入同一 fuel；输入/输出各限 64 KiB（严格小于旧 512 KiB 通用上限），实例最多 4 MiB memory、既有 table 上限，module 8 MiB；主机上限不可由插件提高。

为使模块解析/实例化、host 调度阻塞和取消可回收，**不依赖 `spawn_blocking` 的 JoinHandle abort 作为终止**：使用 Host 自带的专用 WASM worker 可执行入口，专用匿名管道、随机 nonce 和单次请求，在独立 Job 内执行 wasmi；必须隔离或避免向 worker 传 Host 凭据/DB 句柄。最大并发 2、排队 16、启动 3 秒、单次绝对截止 5 秒（含编译与初始化，可按 Host 配置仅收紧）；排队取消不启动。取消/超时关闭 admission、终止 worker Job 并等待进程和 I/O 实际回收；许可直到确认退出才释放。进程结束无法确认则标记清理 unknown、封闭该运行代，不创建无限新 worker；不得把 caller future 已返回当进程已停止。worker 也必须以原子 Job-list 创建、唯一不可继承 Job 句柄及 kill-on-last-handle-close 管理；Host drop 走有界 shutdown，Host 自身崩溃或被强杀时操作系统关闭 Host 唯一 Job 句柄并终止成员。测试需在 worker 编译前、编译中、初始化、调用和 I/O 阻塞时分别 kill Host，外部观察者保留 PID/进程句柄并确认退出；不得令 child 或任何其他进程继承/复制 Job 句柄。对于 Host 崩溃，无法把清理成功写入已死进程的 DB，恢复只依据原事务和新进程重核，不伪造 receipt。WASM 无 imports 是能力边界，并非可证明绝无 wasmi 漏洞；worker 资源上限和异常退出进一步隔离。

Process 每次决策一个新 child，避免跨调用状态与复用协议串扰。由 Host 启动经身份校验的入口，**只能通过继承白名单中的 stdin/stdout/stderr**，无环境凭据、Host token、命名管道/DB/项目文件句柄；系统 loader 所需系统二进制/自身只读工件另作第 7 节最小权限例外，不授权 workspace。`stdin/stdout` 用 4 字节大端无符号长度 + 单个 UTF-8 JSON，0 和 >64 KiB 立即拒绝；最多握手、请求、响应各一帧，禁止额外 stdout，stderr 并发吸收、累计至多 64 KiB 且从不原样存日志/回显。Host 在私有匿名通道发 `{"v":1,"kind":"hello","nonce":"...","plugin_id":"...","artifact_id":"...","interface":"agent.strategy@1.0.0"}`；子端原样回相同字段与 `kind:"ready"`。握手只检查协议一致与管道关联，**不证明二进制身份**，身份由上文 Host 文件句柄链证明。接着发送第 3 节单请求，匹配 id，禁止第二次响应、截断/非 UTF-8/超长/卡住；边读边施加界限，不先无界缓存。Host 对输出只记录有限错误码与 plugin/artifact 身份，禁止原文、环境、内存、路径、参数、密钥落日志。

固定建议上限：每插件并发 2、等待队列 16、handshake 3 秒、整次 deadline 5 秒、进程内存 128 MiB、CPU 时间 3 秒，任何配置只能降低；进程数上限 1，禁 breakaway。每个进程专用 Job 句柄仅 Host 持有且不可继承，启用 kill-on-last-handle-close、禁止 breakaway；通过 `PROC_THREAD_ATTRIBUTE_JOB_LIST` 在 `CreateProcess` 创建时原子放入预先配置完成的 Job，配合 suspended 启动和受限 token/白名单句柄；确认 Job 成员与隔离后才 resume。不能先创建悬挂进程、再事后分配 Job：Host 若在两者间被强杀会遗留孤儿。目标系统/API/宿主既有 Job 策略不支持原子入 Job 时 fail closed；所有设置或验证失败先终止并 wait，绝不继续。取消、超时、协议错误/异常退出先停止接受结果，关 stdin，250ms 有界 grace 后 `TerminateJobObject` 并等待最多 2 秒、回收管道与真实子进程句柄；无法证实退出为 cleanup_unknown，关闭该域并留可诊断状态，不能返回成功、重试或绕过该实例。正常结果也需 wait 证实退出，无后台常驻线程或后代。不得对宿主文件/网络副作用声称 rollback：若隔离失效/证明不足，整个 process 特性不予通过验收。

## 6. 持久化、CAS、升级、恢复

需要 schema 18，不能把选择塞临时 flag，也不扩充 schema 17 `plugin_desired` 闭集/老 DTO。新增六组表并采用与既有 schema 相同的严格 shape/preflight/迁移验证，外键、CHECK、唯一键、版本上界与完整性 fail closed：

1. `external_artifacts(artifact_id PK, plugin_id, plugin_version, runtime, manifest_canonical, installed_at)`：不可变内容身份；目录位置由 Host 按 digest 推导，不存任意可执行路径。
2. `external_agent_desired(plugin_id PK, artifact_id FK, enabled, revision, trust_mode, updated_at)`：某 id 当前期望、准确批准摘要、显式 isolation 策略；内置 desired 沿原表。版本替换或 disable 原子递增 revision。
3. `project_agent_selection(project_id PK/FK, selection_kind builtin|external, plugin_id nullable, artifact_id nullable, revision, updated_at)`：无行等价 builtin revision 0；external 引用已安装身份，独立 project config 原有 DTO。选择修改 CAS + 项目身份/权限检查；只能选择已授权可用工件。
4. `task_agent_binding(task_id PK/FK, selection_kind, plugin_id nullable, artifact_id nullable, selection_revision, desired_revision, created_at)`：在 run/turn/task 创建和 idempotency receipt 同一个 SQLite IMMEDIATE 事务中写入，不是运行时可变快照；旧 schema 17 已有任务无行解释为 builtin，禁止回填成后来选择的外部工件。
5. `external_agent_requests(key PK, op_kind, request_digest, receipt_json, committed_at)`：保存不可变的管理命令接受 receipt；key 同内容重放原 receipt，异内容冲突。revoke 的接受 receipt 明确为 `requested`，不能当作已完成。
6. `external_agent_leases(lease_id PK, task_id FK, plugin_id, artifact_id, desired_revision, owner_instance_id, owner_pid, owner_process_creation_time, state active|completed|cleanup_unknown, created_at, ended_at)` 与 `external_revoke_operations(operation_id PK, plugin_id, requested_revision, state requested|completed|cleanup_unknown, updated_at)` 可合并为第六组生命周期表：用于跨 Store 证明尚有占用及持久撤销状态。owner instance 是每次 Host 启动的随机身份，PID 必须搭配创建时间；lease 不保存管道内容或秘密。任意不能证明已退出的 owner/worker 不可被当作已释放。revoke 观察 status 是独立只读查询，receipt 不含路径或进程输出。

任务新建事务同时验证项目当前配置、策略 selection 与 external desired/摘要；已有 idempotency key 重放返回原 task/binding，不按当前选择重写。项目切换只改变未来任务；已提交 task 固定身份。`acquire_persisted` 按固定顺序持原 Agent master 短锁→对应外部域短锁，再以**同一个 SQLite IMMEDIATE 事务**读取 task binding、冻结的 project selection revision/identity、`plugin_desired('builtin.chat-agent').enabled` 总闸门及 external enabled/revision/artifact；task binding 写入后不可变，不要求当前项目 selection 仍相同，但 external desired 必须与冻结 revision/digest 相同。仅事务内各条件全部成立才插入 durable active lease；事务提交后在仍持两域锁时同时建立 master/external in_flight guard 并发 lease；若 guard 建立失败，以补偿事务终结刚创建的 lease，补偿不确定则标记 cleanup_unknown 并封闭该域；与跨 Store 的 builtin disable、external disable/upgrade/revoke 的写事务由 SQLite 序列化，提交前关闭的必拒、准入事务先完成的 lease 按 drain 规则处理。必须实现并测试此线性化点，不能先独立读总闸门、释放事务再查 external；持久 task snapshot 与选择创建也必须在同一事务。锁序固定 master runtime→external runtime→Store；旧内置路径保持 master runtime→Store，禁止持外部锁反入 master、禁止持任一锁等待进程 I/O 或执行回调。receipt 表、选择和 desired 是各自事实，不复制到新权威。

升级以新 digest 安装，CAS 更新 desired，原代关新 admission 并 drain 已发 lease；新任务用新代，已提交但尚未取得旧代 lease 的 task 因 desired/digest 不匹配 fail closed，需要用户显式新建任务。普通 disable 停止新 lease、已准入 lease 维持现有 Agent 语义；新增独立 `revoke` 表示强制取消已准入外部决策并回收 worker，分两段：第一事务 CAS 置 desired disabled、revision++，登记 `revoke_operations.requested` 和不可变 `requested` 接受 receipt；事务提交即阻止所有后续准入。锁外通知已登记 lease 并 kill+wait，单个 lease 只有真实 worker 退出和所有 I/O 回收确认后才事务置 completed；全部有关 lease 已 completed，才能第二事务将 operation 置 completed。调用方可查询 operation，重复同 key 返回原请求 receipt 并查询当前状态；不得提前返回完成。若 kill/wait、持久提交或 owner 生死不可证实，事务置 `cleanup_unknown` 并持续拒绝该域重新启用，绝不能只靠内存封闭。Host 强杀后，恢复时结合单实例锁、owner PID+创建时间、不可继承的 kill-on-close Job 证明和保留的进程证据核对；无法证明的 durable active lease 降为 cleanup_unknown，等待另行定义的显式人工核验/解除流程，不自动标 completed 或重启插件。旧版 builtin disable API 及语义不变。

主进程重启时仅重建启用且被选择的绑定；disabled 工件不打开/执行，故缺文件不阻止 Host 和 builtin 启动；启用项目工件缺失/哈希错误/隔离不可用时该选择报 unavailable，不自动激活 builtin。旧 Host recovery 的网络、审批、文件/命令 claimed/unknown 事实保持；外部纯决策没有可重放的外部副作用，未完成 task 仍按既有 interrupted/reconcile，不自动再次发网络、写或命令；用户 resume 的旧外部 task 必须仍能验证原 digest 和 desired revision，否则明确 stale，禁止静默改用当前包。进程代 `generation` 是本次进程短期事实，不作为跨重启授权；DB revision 才是权威。

## 7. Windows 隔离先决试验及诚实支持边界

候选为每个 process 域使用 Windows LPAC 安全属性（`SECURITY_CAPABILITIES`、零显式 capability、`ALL_APPLICATION_PACKAGES_POLICY` opt-out），专用 AppContainer SID、专用只读工件 ACL 和隔离的空工作目录；持久 Host 数据/密钥/DB/工作区不向该 SID 授权。启动只继承 `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` 指定三条匿名管道，句柄不可用于访问主机其他对象；受限令牌、AppContainer SID、integrity、capabilities、ACL 和主进程/子进程属性均以运行中对象检查，而非只检查 API 返回。无网络 capability，再以实际目标系统验证阻断公网、DNS、回环和代理；若操作系统规则不能保证，须实现并证明额外 OS 级拒绝策略或拒绝加载，不把用户态协议过滤当防火墙。

Job 用 kill-on-close、active process count 1、内存/CPU 限制；通过 Job-list 创建时原子分配 Job 并保持 suspended，验证后才 resume，禁 breakaway；不能依赖事后分配避免派生或 Host 中途崩溃。Job 管生命周期，不授予/限制文件和网络权限；安全 token/LPAC 约束每个 process，不能只给 Job 设置。以负控实际创建文件/打开任意用户资料、SQLite/WAL、DPAPI 相关材料、已知工作区、其他项目、系统/用户目录、注册表和跨进程句柄、连接 loopback/公网、创建子进程、尝试继承句柄；用子进程内部观察与 Host 哨兵共同证明。能访问的数据目录、公共 ACL、系统 runtime、同用户宿主进程和 LPAC 默认可访问对象必须列出；不得假设 LPAC 自动隔绝所有世界可读文件、DLL 载入或防御恶意管理员。

必要系统 loader 对自身工件及系统运行库的读取与“插件不能直接访问文件系统”字面要求之间存在待审边界；本轮不将 LPAC API 成功等同满足项目书。经理已提出只允许自身工件和必要系统 runtime 的最小只读 loader 例外，正式契约须冻结其范围及“不能访问 Host Key/SQLite/用户文件/工作区/网络/任意进程”的可检验能力清单；若项目书要求连任何系统文件读取也禁止，LPAC 单独不足，必须另设计更强容器/虚拟机或禁用 process，不能声称完成。目标 Windows 11 Home 10.0.22631 + SDK 22621 需以真实机器试验验证；Windows API/系统策略/文件系统不支持、ACL 无法设置、无法关闭网络、工作目录或 Job 无法强制、行为不确定、非 Windows，均对 process 返回安全 unavailable，Host/builtin/WASM 可启动，**绝不降级为普通进程**。参考 [Microsoft LPAC/AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer) 与 [Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)；两篇文档只能支持 API 设计，不能代替本产品的攻击证据。

## 8. 分阶段、唯一写入与验收

本报告不派发员工、也不授权实现；下表是经理冻结时建议的**每阶段单一写入人**，采用新独立树并由经理指定 B 或其他唯一 owner，不以测试目录划分制造同文件并发。阶段内一个 owner 独占所有列出文件；经理唯一整合。

| 阶段 | 必须落地的授权文件（以 `NoManCode/rust-app/` 为根；新增均列出） | 可验证交付 |
| --- | --- | --- |
| A：隔离可行性 | 新 `src/external_process_sandbox.rs`、`tests/external_process_isolation.rs`、测试夹具源码与 `Cargo.toml`/`Cargo.lock` 必要 Windows bindings；经理批准明确夹具路径 | LPAC/ACL/handle/Job/网络负控、失败场景及不支持返回；不接业务、不暴露误导性的已启用接口。需验证新增 windows-sys feature 名与 SDK/离线依赖，必要时登记 lock 变化，不假设现有 0.59 features 已覆盖。 |
| B：核心/WASM | 新 `src/external_artifact.rs`、`src/external_agent.rs`、`src/agent_protocol.rs`、`src/wasm_worker.rs`；现 `src/plugin_host.rs`、`plugin_catalog.rs`、`agent_runtime.rs`、`engine.rs`、`composition.rs`、`host_services.rs`、`wasm.rs`、`store.rs`、`repository.rs`、`lib.rs`、`main.rs`、`server.rs`、`cli.rs`、`Cargo.toml`/`Cargo.lock`；新对应 tests 与示例工件源码 | schema18 原子迁移、真实绑定与 task snapshot、WASM normal-main E2E、旧 DTO/CLI 回归。仅一次唯一 owner 写共享 Engine/Store，CLI 新命令在同阶段统一所有权；测试由经理另派不重写生产。 |
| C：process 接入 | 复用 A 的 sandbox、B 的工件与 adapter，独占 `external_process_sandbox.rs`、`external_agent.rs`、`agent_protocol.rs`、`plugin_host.rs`、`agent_runtime.rs` 及必要注册/CLI/测试文件；经理按实际 diff 最终列唯一写权 | A 阻断项全部闭合后，normal-main 真实项目策略走 LPAC process，协议/取消/重启及不降级闭环；若 A 失败此阶段不可宣称通过。 |

必须在独立树按经理冻结版本把 schema 修改、旧数据迁移与现有测试更新作为同一次可回滚提交；不在 schema17 中绕开检查或降低既有安全/事务断言。A/B/C 可以作为可独立 review 的提交序列；是否串行为经理裁决，跨阶段的共享文件不得多 owner 同时写。测试内容至少如下，必须记录真实入口、固定候选、原始日志和 Windows 证据：

| 组别 | 直接证据 |
| --- | --- |
| 旧兼容 | 原 schema17 带旧任务、审批、网络与结果 receipt 迁移到18，旧 task 一律 builtin；旧插件列表/运行域列表精确两项，旧 CLI 严格解析及原 first_user_workflow/tool_call_workflow 通过。 |
| 工件 | 一字节变化、错误 digest、同名换包、错 manifest/version/interface/kind、缺依赖、重复/逃逸/reparse/hardlink、持锁替换/TOCTOU、缺文件、超限；拒绝且零 effect/子进程，跨重启结果一致。 |
| 接线 | normal main 安装启用与项目选择→真实 Run/Session 新 Turn→Engine Before/After→真实外部 agent；示例实际过滤命令或拒绝写，原用户审批/文件哨兵证明不能扩大权限；id/digest 与 task binding 可观测。 |
| 生命周期 | start 失败部分 effect 逆序回收、错误类型不可用不 fallback；disable 与在途 lease、重启后 persisted desired、跨 Store 旧 Agent 总闸门及外部 desired 的准入/disable 竞争，master guard 的 in_flight/draining、持久 active lease 与 revoke requested/completed/cleanup_unknown 崩溃重开、CAS/receipt 重放/冲突、升级中旧代 drain、新代不同 digest、revoke 等待回收。 |
| WASM | 初始化循环/fuel 耗尽、memory/table/模块上限、import/WASI、错 ABI、无效指针/UTF-8/闭集/长度、初始化和运行超时/取消；worker 真实 PID 被 kill+wait、队列上限与许可释放；Host 在编译、初始化、阻塞 I/O 及创建交界被实际强杀，外部观察其进程与后代均退出。 |
| process | 第 7 节真实攻击反例（含 DB/WAL/key/workspace、公共 ACL、网络/DNS/loopback、句柄和子进程），suspended→Job→resume 序列；缺 LPAC/策略拒绝启动，日志不暴露输入；后代不能逃逸。 |
| wire | 错 nonce/id/version、重复帧、乱序、截断、超长/慢输出、无效 JSON、错误工具索引/票数、早退出、挂死、stderr 洪泛；CPU/memory/并发/timeout/取消逐项有 PID/回收证据；无任何无界缓存。 |
| 恢复 | 宿主在 agent 调用、审批 claim、命令/文件执行前后分别实际 kill/restart，验证既有 unknown、不自动重做副作用；resume 原摘要缺失/禁用/升级明确 stale，不冒充 builtin。 |

格式/Clippy、workspace check/test、显式 CLI build、UI wasm-check 和公开端到端由经理在最终联合组合上安排；用户最初要求的完整项目剩余目标不能由本设计报告代替。

## 9. 未决与交付说明

经理已明确提出第 7 节最小只读 loader 例外，须纳入正式契约并以实际 Windows 先决反例证明；不成立则 process 保持不可用，不能把可信本地进程作为替代交付。用户对 process 的产品信任模型、是否允许策略项目级选择与原 CLI 的可发现性由经理按用户授权冻结；本方案给出可实施默认，不需要为普通实现操作再次征询许可。文中参数是拟冻结的上限，不是现行行为；没有执行构建、验证或安全测试。B 本轮只读设计结束后释放，待经理审查、冻结基线/唯一写权及阶段 A 实验后才可实施。
