# NEXT-21W3 全入口执行权实施契约（C-R34）

2026-11-01；员工 C。固定 main `56f471c4df6ca0e8e589c3f43072ca2a2614c0ef`、产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58`、Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`。只读契约建议，不是已实现的保障；未运行测试、构建或进程。

## 现有入口及权限表

| 固定代码入口 | 实际行为 | 建议权限 |
| --- | --- | --- |
| `src/main.rs:87-107,125` 正常启动 | 建目录、对目录内 `instance.lock` 尝试文件锁，再打开 Store、初始化设置、恢复和启动 Engine；锁只由 main 局部持有 | 统一域执行权；生命周期由统一 owner 承接，而非只由 main 局部变量保护 |
| `main.rs:94-123` `--check` | 跳过 instance.lock，但 `Store::open` 会建表、WAL/迁移（`store.rs:61-73`），缺设置还可 `fresh_settings`/`initial_settings`；不构造 Engine、不 recover | 建议拆成真正只读检查；若保留创建、迁移与初始化，则先取得同一维护/执行排他权，活 Host 时明确 busy，不能同时声称无写入。其输出应区分只读可校验和初始化可启动，不伪报 |
| `Store::open` 和独立 Store | 目前公开读写连接、初始化和迁移，实例内 `Mutex<Connection>` 只管这一连接；多个连接合法，SQLite 事务负责数据库互斥 | 分离 `open_readonly`、持域排他权的 `open_or_migrate` 和已验证 schema 的普通连接；只读连接不创建/迁移、不恢复、不持执行权。合法多连接不应被无差别禁止 |
| `Engine::new` / `new_recovered`（`engine.rs:779-797`） | 前者不 recover 仍创建可执行 Engine；后者调用 `RecoveryService` / `Store::recover` 后创建；每个 composition 分配自己的 gate/active/slots（`:904-938`） | 两者均要求同一有效 owner；默认无 token 的公开构造不能保留可启动执行的后门。一个域在进程内也只允许一个执行 controller，其他连接共享只读/受约束 Store，不能把可 clone owner 当成多 Engine 派生授权 |
| `Store::recover`（`store.rs:2254-2314`；`host_services.rs:982-996`） | 公开入口会把 queued/running 中断，处理网络/审批/workspace 等 unknown，可由未拿 main 锁的调用方触发 | 仅 booting/recovering owner 可执行，在同一事务验证 owner epoch、封门及域；运行中二次 recover 不可并发破坏活跃执行，必要管理恢复应先安全停机并按新 epoch 进入 |
| 测试/嵌入 | `engine.rs:799-850` 有 cfg(test) 自定义 runtime 构造；生产 `Engine::new` 和 `Store::open/recover` 为公共入口，集成测试亦直接调用 | 通过相同 owner 接口使用真实临时域；只有只读与纯事务测试可使用无执行能力 Store。测试桩不具备可编进生产的 bypass |
| CLI | `main.rs:56-78` 走远程/本机 HTTP 客户端后直接返回，不直接初始化 Store | 仍通过已授权 Host 命令；不另拿数据域执行权 |

SQLite IMMEDIATE 排他的是数据库写入事务，不是该事务前后跨进程 OS 工作的运行权；Engine gate/active 是单 composition 内互斥，不能约束另一个 Engine、Store 或进程；OS 域 guard 是 Host 存活期间独占，DB epoch/fence 是拒绝过期持久提交的第二层。这三者均不能单独证明旧 worker 已退出。

## 统一 owner 接口（示意，非已实施）

```rust
struct DomainId { /* 实际数据域对象身份；内部构造 */ }
struct ExecutionOwner { /* 私有 OS guard、domain、epoch、gate */ }
struct BootOwner { /* 独占 guard，仍封门，尚不可执行 */ }
struct OwnerRef { /* 私有 Arc；非可序列化、不能由 epoch 自造 */ }
fn acquire_domain(data_dir: &Path) -> Result<BootOwner>;
fn open_or_migrate(boot: &BootOwner) -> Result<Store>;
fn register_epoch_and_seal(boot: BootOwner, store: &Store) -> Result<ExecutionOwner>;
fn recover(owner: &ExecutionOwner, store: &Store) -> Result<RecoveryReceipt>;
fn activate(owner: &ExecutionOwner, receipt: RecoveryReceipt) -> Result<OwnerRef>;
fn open_readonly(data_dir: &Path) -> Result<ReadStore>;
fn engine_start(store: Arc<Store>, owner: OwnerRef, ...) -> Result<Arc<Engine>>;
```

接口名、类型可调整，但 owner 私有构造、域绑定、执行位阶、不可由公开数据库整数伪造和同域单 controller 为必需约束。`OwnerRef` 只传播给 Engine 和其派生在途工作；仅 registry/controller 可启动一个 Engine，不是每个 clone 都能创建独立 Engine。普通连接可绑定同一 owner 进行事务，但不能注册第二 owner 或用同一个 token 启动平行 recover。公开 `Engine::new` 必须签名迁移或内部获得真实授权并强制唯一，不可保留无验证形式；`new_recovered` 同理。`RecoveryService`、`HostComposition` 与测试注入构造全链传递并校验同域，而非仅 `Arc::ptr_eq` Store（`host_services.rs:991`）。

### 域身份和替换

建议对现有本机 SQLite 数据域明确限定：在取得/bootstrap guard 前定位数据目录的真实对象身份；打开并固定域目录、锁对象及数据库实际文件对象，按卷/文件 ID 等 OS 对象身份而非原始路径字符串比较，并在连接建立及每次重新打开时核实仍与持有句柄同一对象。对目录别名、大小写、相对路径、junction/symlink 等指向同一域必须汇聚到同一 OS guard；数据库硬链接或把同一个 DB 放到另一目录的情况，如不能让所有入口获得同一不可替换的 guard，则显式拒绝该部署，不能仅 canonicalize 字符串后宣称安全。锁文件被删/替换、DB 文件被重命名替换、guard 和数据库失配、难以确认文件身份或 SQLite 实际打开对象时应拒绝或封门；不要在已持旧对象锁时悄悄改锁新对象。实现必须定义缺库首次创建的 bootstrap 顺序并保持 guard 到 DB 身份固定；打开对象时要选用能防止并检测重命名/替换的共享模式，若系统不能提供则 fail closed。并发测试应直接验证这些前提，不假定标准路径规范化或 SQLite 锁已覆盖它们。

需要注意：同一个数据库通过不同目录硬链接而形成不同 sidecar `instance.lock` 时，目录锁各自成功并不构成独占。需要可验证的 DB 对象身份级统一锁/注册，或在建立执行权前可靠拒绝这种身份；只写“数据库 file ID 相同”而没有跨进程统一仲裁同样不够。域 UUID 可辅助检测误接入/回滚，但复制数据库的 UUID 不构成文件对象同一或真实独占证明。跨系统重启、克隆与还原的证据仍受 C26/W3 边界约束。

## 事务和生命周期顺序

1. 按域规范取得唯一 OS guard 并验证物理身份；在 guard 下打开/必要迁移数据库，不能先调用有副作用的 `Store::open` 再竞争执行权。正常 startup 和可写 `--check` 均经此门。获取失败或身份不可信：返回 busy/错误，绝不 recover 或启动任务。
2. `BEGIN IMMEDIATE`，核对域绑定/owner 元数据且验证本地 guard 尚有效；单事务递增单调 epoch、设置 phase=`recovering`（封新创建/claim/结果采纳），对溢出/损坏 fail closed，commit 成功才向后推进。崩溃发生在提交前视为未取得新 epoch，提交后视为 recovering，下一合法 owner 取得新 epoch 并重新进行恢复；不能以 epoch 的存在认为上一 owner 已安全清理。
3. owner 在同域事务校验精确 epoch/phase，执行现有 `recover` 与 worker/scheduler 未来的未决记录分类；已完成保留、已领取不确定置 unknown。恢复可拆事务时每步幂等且持续封门，最终在校验所有必要恢复步骤完成后事务提交 phase=`active`。不因调度可用或 owner 死亡清除 W2 未决占槽；若旧 worker 是否已终止不可证，其资源域保持 unknown，即使其他独立安全业务可继续也不得对它重新创建或宣称回收。登记或恢复失败不发布可运行 Engine；保留 guard 直至所有已经启动的本进程 OS 操作安全退出。
4. active 状态下，事务必须验证 owner 能力绑定的 domain+epoch+phase，再提交任务创建/执行准入、网络及工具 claim、worker creating/registered_suspended 状态与结果采纳、scheduler claim/终态提交以及相关恢复写入；条件更新检查行数，失败即拒绝。需要将 owner 带到异步 closure、spawn_blocking、supervisor 和实际结果提交，不能只在顶层 start 验一次。对不创建执行权的普通配置、只读查询、授权/审批用户决定等，保留现有权限、事务和并发语义；但任何能触发副作用、改运行生命周期或使未决操作重入的写操作都必须经 owner/fence。正式实现应逐个审计所有公开 Store、composition 和 server 写路径，不只套一个 `recover`。
5. 接受 OS 调用前在同一域准入门下验证 owner 且登记 in-flight；关闭门后不再授票，已授票操作的真实句柄和 guard 活到其系统调用及必要回收/观察结束。不可先 release 文件锁、靠 epoch 把仍在跑的旧线程当隔离。若 OS 调用无法结算/取消失败，则同进程保留 guard 和资源、封门并呈 unknown；进程异常终止后 OS 会释放 Host guard，但外部 worker、已经发出的 Provider 请求/工具副作用仍可能存活或结果未知，不能因新 owner 取得锁自动重发。无法追踪的错误、DB commit 失败、owner 失效或校验不一致均拒绝接纳新操作，记录可观察错误；原子状态不确定不能假定回滚。

对当前固定主线的 Engine `start`、`start_idempotent`、`start_in_project`、`send_chat_turn`、`resume`、`cancel`（`engine.rs:1475-1505,1706,2217,2306`）及启动生成的 task/工具/网络/工作区保存入口，都需按照作用审计迁移；REST handler 保持现有用户语义，不能让它绕过 Engine 新 owner。对未来 W2 创建设备与 W3 恢复、未来 Scheduler claim 复用这一个 owner/epoch，不能各自建立互不约束的 owner。

## 兼容迁移与直接验收

- `composition_tests.rs:160-176` 当前两次 `new_recovered`；应改为先安全释放第一个 Engine/所有执行工作，再取得下一 owner，保留重启恢复及二次恢复幂等断言。并发执行者必须单独验证被拒绝。
- `store.rs:5170-5193`、`budget.rs:1211-1213` 等直接 `recover` 的测试迁移到持权恢复夹具；对只验证事务幂等、无真实活执行者的测试可显式建立 owner，不允许设置测试开关绕过新保证。`engine.rs:4056` 同 Store 第二 Engine 场景须按用途改为共享同一 controller 或验证拒绝；不得破坏合法多 Store 读取/事务竞争测试。真实强杀例 `engine.rs:6485` 应由子进程真实获得执行权、父进程等待其退出后取得新权，保留网络未知不重发断言。
- 两个 Store 同域：一个运行 owner、另一查询/普通合法事务可用；第二 Engine/第二 recover 必须失败。两个真实 Host 同域同时争夺，至多一个取得运行权；目录别名及硬链接/替换按支持或拒绝策略验证不能双 owner。测试路径身份变化前后持有真实文件对象并比较，不只比字符串。
- 活 Host 与 `--check` 并发：只读检查不改 schema/settings/recovery，缺库或旧 schema 明确报告未验证；若选保留初始化，则检查模式得到 busy 且记录未改任何行。新域 `--check` 需按最终产品约定验证可见行为，不能暗中改变原 CLI 契约。
- 真实强杀分别在持锁登记前、epoch 提交后恢复前、恢复提交前后：确认下一次重新取得实际 guard、epoch 单调、封门/unknown 和事务原子性；旧 epoch 迟到尝试写入要失败，且有独立证据 OS 调用没有在新 owner 授权后仍可不受控创建。仅模拟 epoch 对比不够。
- 测在途调用取消/失败时 guard 不提前释放、结果提交失败不判成功；worker 句柄仍待收割时新建被拒，owner 死亡但真实旧 worker 无终结证明保持 unknown。验证查询与执行隔离、同键请求与人工新消息语义分离；本切片通过仍不等于 W3 清理证明、W05/W06 通过或完整持久 DAG 续调。

下一步等待经理核收并冻结唯一写入人/实现切片；不重复 C26 启动代证据、C29 工件或 C33 Team 全设计。
