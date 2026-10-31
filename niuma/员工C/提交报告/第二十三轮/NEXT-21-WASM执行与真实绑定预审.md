# NEXT-21：WASM 执行与真实绑定预审

2026-11-01 · 员工 C / 6.1 Terra · 第二十三轮，只读预审，供经理裁决，不授权实现。

## 一、具体发现

1. **P0：`validate_module` 是执行，不只是结构验证。** `rust-app/src/wasm.rs:129-173` 对输入调用 `Module::new`、检查 import 和三个导出名称，再创建有限 Store、设置 fuel=1，调用 `instantiate_and_start`，之后才读取 memory 并校验最小/最大页数。start 会在此阶段运行；1 fuel 不能构成解析、实例化、内存初始化的 wall-clock 或进程资源上限，也可能拒绝带有非平凡 start 的合法模块。当前校验仅检查 alloc/run_json 导出名称，签名在 `run` 的 `get_typed_func` 才验证。不能把它放在 Host 内当无执行的安装预检。
2. **P0：现有计时与取消不覆盖全链。** `run` 先 `validate_manifest`、`validate_module`、序列化输入，随后才 `Instant::now()`；再重新创建引擎、解析、实例化/start、执行函数（`:176-216`）。因此 `elapsed_ms` 排除先前校验、首次编译和初始化，并不是总期限。`load` 先整文件读取再校验（`:220`），`run` 又校验一次；每次调用至少两次解析和实例化/start。fuel 和内存/表限制有用，但不是 Host 整体时间限制，也没有证明恶意解析器输入可安全停机。
3. **P1：旧兼容实现存在，但正常 Engine 已有拒绝门。** `workspace.rs:341-349` 的 `run_wasm` 用 Host 进程 `spawn_blocking` 调用 `load` + `run`；`tool_runtime.rs:32,619-629,686-690` 仍作为 compat 工具提供，`approval.rs:172` 分类为无效果。然而正常 Engine 进入 `tool_call_application.rs:192` 的 `execute_or_wait` 首先对 `run_wasm` 返回 `OperationUnsupported`；`tests/tool_session_turns.rs:825` 静态记录此预期。旧 `/api/wasm/*` 也显式 unsupported（`server.rs:2471-2495`）。所以不能从兼容定义推断生产 Engine 可执行，也不能称其为已证明的生产旁路。低层直接调用 `workspace::execute` 等仍有进程内执行语义，未来开放任何新调用不得绕过现有拒绝门；阻塞线程 future 取消不等于终止执行。
4. **P1：现有工厂与目录不能直接接真实外部策略。** `AgentRuntime::assemble_with_state` 只登记 `builtin.chat-agent`、builtin consumer 和 `StrategyPlugin`（`agent_runtime.rs:235-260`）；`manifest` 固定 RuntimeKind::Builtin（`:115-145`）。`PluginHost::start` 的 `:1030` 显式拒绝所有非 Builtin；`with_factory` 接受 `Box<dyn BuiltinPlugin>`。即使登记外部 manifest 而不修改激活路径也只会 UnsupportedRuntime。更不能给外部工件套 builtin manifest 或借 `with_strategy_for_test` 冒充生产接线。
5. **P1：同时登记两个同接口 provider 不能选择项目绑定。** `plugin_catalog.rs:932-1039` 对同一精确接口多 provider 报 AmbiguousProvider，不按登记顺序选。`resolve_bound`（`plugin_host.rs:690-750`）读取激活计划、effect owner 和类型，但当前 `AgentRuntime::acquire_persisted`（`agent_runtime.rs:325`）取 payload 时丢弃 `provider_id`，没有核对绑定身份。真实调用必须显式限定所选 provider 并复核返回 owner；仅在返回 DTO 中声称外部 ID 不能证明绑定。
6. **P1：同步策略签名不适合可取消的进程调用。** `AgentStrategy::decide`（`agent_runtime.rs:83`）同步且收到原始 `messages`；`AgentLease::decide`（`:641`）通过同步 `guarded` 调用并验证阶段、索引、有序性、投票数量和 Stop。`engine.rs:1992,2105` 两处同步调用。直接在其中等待 worker 会阻塞异步执行线程；直接把参数序列化则泄漏消息。第二次决策前 assistant/tool-call group 已保存（`engine.rs:2085`），失败或超时后必须走现有恢复语义，不能重放已发生的 Provider/工具效果。

## 二、推荐实现边界

### 安装、编译、调用及限时

- Host 仅处理有界封装和身份：先按硬上限限流读取/拒绝超限，限定 manifest 字段、协议版本、摘要和授权；计算 digest 并将同一不可变字节快照送 worker，禁止校验后按可变路径重新打开而发生替换。Host 不解析/编译/实例化不可信 WASM；本地 `wasm.rs` 里的 `validate_manifest` 的字段、摘要规则可提炼复用，但 `load` 的无界 `fs::read` 和随后的 `validate_module` 不能原样作为 Host 安装路径。输入大小上限在分配之前执行，manifest 也须单独限长。
- worker 受监控启动之后才执行 `Module::new`、导入及导出类型检查、Store 限额、实例化/start、`alloc`、`run_json` 和 JSON 解码；此处可复用 `wasm.rs` 的无 imports、`StoreLimits`、fuel、指针/输出长度检查及 ABI，但重构为一次编译和一次实例化。检查导出类型、memory 初始/最大及表限制时先尽可能基于编译后元数据检查；即便纯结构解析也运行在 worker 期限内。只有能准确保持行为且经过验证时才复用现有函数；`validate_module` 不是纯检查，不要求一 fuel 虚假运行 start。`MAX_MODULE_BYTES=8 MiB`、`MAX_JSON_BYTES=512 KiB`、`MAX_MEMORY_PAGES=64`、`MAX_FUEL=50,000,000` 当前值见 `wasm.rs:13-19`，为上限参考，不证明总资源安全。
- Host 的单次调用 wall-clock 截止从准备启动（包括创建隔离域、子进程与传输）之前开始；覆盖发送、编译、start、分配、运行、读取和退出回收。启动失败、超时或取消要关闭管道、锁外取消/终止隔离域并 wait；只有得到可信退出证据才能声明回收成功，否则按第二十二轮 unknown 处理。采用 OS 资源限额和独立 supervisor/watchdog，使 Host 自身失活时也能落实必要策略；A 对原子 Job、句柄继承、可观测退出与相关机制尚未证明，不当成已实现。锁外执行，不在 SQLite 事务或 AgentRuntime mutex 中 await。
- 内置 worker 由 Host 的固定受控入口启动，而非 manifest 指定路径/命令或环境驱动任意程序。单进程/单请求，长度前缀帧在分配前检查（分别限制 module、DTO、回复；总和有硬上限），版本、request ID、随机 nonce、binding generation 全部严格匹配；拒绝重复、截断、超长、未知字段、额外帧、无效 UTF-8/JSON、异常退出和迟到结果，stdout 只作协议，stderr 有界丢弃或脱敏。worker 仅接收受控模块字节及最小纯策略 DTO：阶段、工具类别列表、round/max_rounds；不传消息、路径、Key、环境凭据、数据库句柄或工具参数。继承句柄和环境按最小白名单，不能因协议纯净就假设进程隔离成立。

### PluginHost 到 Engine 的真实路径

1. `plugin_host.rs`：将仅允许 builtin 的生命周期工厂推广为**受信 Host adapter 工厂**，按 manifest runtime 显式分发 `Builtin` 和受控 `Wasm`，其他包括 Process 保持 UnsupportedRuntime；factory 创建 Rust adapter，adapter 本身只持绑定身份和受限 worker client，绝不在激活中运行外部代码。维持失败回滚、effect 生命周期与 stop 顺序；`PluginContext::register_effect` 由此 adapter 以真实外部 manifest/plugin ID 注册 `Strategy` typed payload，不能把 WASM 字节视作 Rust trait 或授予 Host 句柄。
2. `plugin_catalog.rs`、`agent_runtime.rs`：为一次绑定使用受限 catalog（仅 builtin consumer + 被持久 binding 指定的唯一 provider），或设计经过约束的显式 provider 解析；不可放宽全局 ambiguous 检查或按顺序选。外部 manifest 标记 `RuntimeKind::Wasm`、精确 `agent.strategy@1.0.0` 与真实 ID；`resolve_bound::<Strategy>` 返回的 `provider_id` 必须等于不可变绑定身份，配合 digest/版本检查。master builtin Agent guard 单独保留，外部域单独持有至 Task 执行结束；不得将外部 provider 塞入 builtin composition 或更改旧闭集。
3. `composition.rs::HostComposition::builtin/new` 及 `engine.rs` 构造/运行接线：注入经审核的外部 runtime/worker service；`run_task` 从该 Task 的持久绑定选择对应类型，获取 master 与外部 lease，直到执行和清理退出。身份、授权、撤销和恢复遵循[第二十二轮报告](../第二十二轮/NEXT-21持久绑定与恢复独立设计审查.md)，不借当前项目选择替换旧 Task。
4. `agent_runtime.rs::AgentStrategy/AgentLease::decide` 改成可 await 的 boxed future 或等价对象安全接口，保持 builtin 的同步计算包装为立即完成 future；把 `messages` 留在 Host/builtin 私有边界，外部 adapter 只能拿到单独构造的拥有所有权的受限 DTO。统一 lease 层在 await 后继续执行现有 action 合法性检查，再进行绑定代、request ID 与撤销 gate 的原子采纳；不持 mutex 跨 await。对于 future panic，需专门处理**异步轮询阶段**的 panic，原同步 `guarded` 无法单独覆盖；错误/取消必须清理 worker 并保留原本 panic 脱敏、failure 和 drop/draining 语义。
5. `engine.rs::run_task` 两处分别 `.await`、传 cancellation/deadline，确保取消与终止/回收完成顺序及 Task lease 覆盖；保持 before 的有序合法索引和 after 的投票数、Finish/Stop 验证，所有工具操作仍经既有 Host 权限与审批，不执行 worker 指令文本。
6. 保持 `tool_call_application.rs::execute_or_wait` 对兼容 `run_wasm` 的现有拒绝以及旧 HTTP unsupported；新 Agent worker 不能调用 Host 内 `wasm::load/validate_module/run`。若将来授权重新开放该工具，须另行迁到受控 worker 并明确旧 JSON 工具输入与纯策略 DTO 的不同权限边界；当前低层 workspace 代码不应被说成正常 Engine 已可执行。

### 建议阶段

- **阶段 0：** B 完成 A 的隔离/取消/崩溃可观测性证据；若尚不能支持安全恢复，后续只可在无生产启用的测试层实验，不视为可发布。
- **阶段 1：** 在同一个受控 worker 边界完成模块传输、一次编译/实例化、协议和总期限；保留现有兼容工具拒绝门。安装验证也在 worker 内。此阶段组件 JSON 成功仅证明 ABI，不代表已接入 Agent。
- **阶段 2：** 显式受限 provider 解析、Wasm adapter typed effect、异步双决策及实际 Engine 接入；与持久 binding、原事务、恢复/revoke 同步接线。没有持久契约与可靠清理前不得开放生产选择。
- **阶段 3：** 通过下表真实调用、禁用/恢复、强杀及旧接口检查后才由经理裁决冻结；保留未知及独立隔离条件，不将 A 暂未通过解释为永久放弃 process 路线。

## 三、验收映射

| 场景 | 必须的证据和结果 |
| --- | --- |
| start 执行、错误签名、memory 最大值缺失、import、巨大声明/模块、异常指针 | Host 未解析/执行模块；worker 内正确拒绝；start 无限循环、编译或初始化耗时均被总 deadline 与资源边界覆盖，可信回收；包含非平凡合法 start，不能被旧 fuel=1 验证误拒 |
| 请求分帧和输入 | 超长长度在分配前拒绝；畸形/多帧/错误 nonce/重复或迟到响应拒绝；adapter 输入抓包不含 messages、路径、密钥、DB、工具参数 |
| 真实 effect/绑定 | 生产工厂加载 RuntimeKind::Wasm；`resolve_bound` 的 owner 为真实外部 ID，版本与 digest 对应；两个同 key provider 不能隐式择一；停止时真实 effect 清除；不能把外部记录为 builtin |
| Engine 策略行为 | 用受控真实 WASM 在 before 去掉一项工具并在 after 对另一项投 deny，观察实际 Provider 入参及 Host tool/审批结果变化；非法索引、错投票、错误阶段、Stop 失败关闭。纯 JSON 回显或测试注入策略不算此项通过 |
| 异步/生命周期 | builtin 原有行为与 panic 脱敏不变；取消、超时、进程启动失败、worker panic/异常退出都终止并 wait 或明确 unknown；依赖等待到两次调用期间双 guard 保持；不能卡 Tokio executor 或持锁 await |
| 撤销、恢复、强杀 | 决策采纳及 effect claim 与 gate 竞争可证实提交顺序；真杀 Host/worker 后依据可信证据清理或标 unknown，禁止重放已 claim Provider/工具效果；原 Task resume 固定绑定 |
| compat 和旧接口 | 覆盖正常 Engine 对 `run_wasm` 的拒绝，低层入口不得被新策略接线绕用；旧 `/api/wasm/*` unsupported 与 composition/plugins/runtime v1 身份和闭集按裁决保持；测试不能只调用旧 HTTP 判断无可达 WASM |

静态来源为仓库固定 `rust-app/Cargo.lock` 的 wasmi 2.0.0 和上述项目源码；本报告未借助外部网页，也未推断库未核实的 OS 行为。核查时 HEAD `eaa144bcb4c7bde2a0f7c907df0a59f64fa5f41d` 等于任务基线；NoManCode tree `897cc8510c0c56118df04c2c1dbcea9c999e539f`、Rust tree `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`，基线到 HEAD 和工作区产品 diff、index/unmerged 均空；人员既有修改保留。本轮未构建、未测试、未运行 Host、worker 或隔离探针；上述为源码事实与待验证推荐，不是动态通过。
