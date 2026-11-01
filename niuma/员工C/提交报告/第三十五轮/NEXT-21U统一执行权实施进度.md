# NEXT-21U 统一执行权实施进度（C-R35，未完成）

2026-11-01；员工 C。独立树 `../nhc-c-domain-authority`，分支 `codex/c/domain-authority`，基线 `688f104b6aa0091068617758ef44d8b097ad5bb5`、产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58`、Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`，开工时 clean。未提交候选。

仅新增未跟踪且未注册的 `NoManCode/rust-app/src/execution_authority.rs` 草稿：从已打开文件取卷序列号/文件 ID，专用线程尝试获取同名 Windows mutex，并有硬链接对象同进程竞争测试源码。此草稿没有目录/数据库 pin、首次建库 bootstrap、SQLite 连接身份验证和完整锁生命周期，不能保证域唯一，也未注册编译或接入产品。互斥名与系统 ACL/命名空间语义及 crash/abandoned 仍须验证。

schema18、Store/Engine 全入口接线、事务内 epoch fence、单 controller、recover 封门、异步 Provider 响应体/工具/子进程与 spawn_blocking 持权、真实双 Host 和全部直接验收均未实施。Cargo/lib 注册已撤回以避免未完成代码误入产品。未构建、运行测试或探针；`git diff --check` 返回成功（未跟踪文件不在其检查范围）。该任务未完成，不能交动态窗口或验收。

下一步继续从真实 DB 文件身份统一仲裁和可证明的目录/数据库持有开始，接入所有受影响入口并补齐测试，再按经理安排固定候选。人员身份已同步；未修改主仓产品或操作暂存区。

## 2026-11-01 同角色接续：SQLite 域保护静态节点（未完成）

本轮执行者 `01a197a8-57b7-7541-abb3-29a4ec0d3ff9` 的实际 turn_context 为 `gpt-6.1-terra/high`。独立树 HEAD `688f104b6aa0091068617758ef44d8b097ad5bb5`、产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58`、Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`；index 空。接收时 Cargo.toml、lib.rs 和 execution_authority.rs 在途，其中后者恢复期间 Astra 草稿 SHA256 `147451294AED542026272EA6F4F341E454E2892932AC667DD130110E644C55F4`、17417 字节，本轮未冒称其贡献。

本轮在草稿上增加受 pin 的 SQLite open 与 SQLite 报告的 main 文件名校验、硬链接观测后删除仍封门的断言，以及同测试二进制子进程写入且关闭自动 checkpoint、提交后由父进程强杀、检查 WAL 文件、硬链接别名拒绝并在原路径读回数据的测试源码。目录组件以不共享 delete 的句柄 pin、固定 NTFS 磁盘、文件 ID mutex 和 instance.lock 的原草稿策略仍待静审/动态验证；SQLite 文件名核验不是已验证的 SQLite 实际句柄身份。`rustfmt --edition 2024` 成功；未构建、测试或启动进程。新增文件未被普通 `git diff --check` 覆盖。

阻断：现有 `verify_database` 只是离散读取链接数；两个检查间出现并消失的硬链接可能漏检，不能声称满足“持有期出现则永久封门”。另所有公开 Store 仍直接打开用户路径、迁移前未取得域权；schema18、Engine/recover、Provider/工具和在途 guard 均未接线。首次建库与 SQLite 打开对象/sidecar 的完整竞态证明未完成。此节点不是完整候选，不交动态窗口、不声称保护生效。

下一步第一操作：确定在整个持有期阻止或可靠捕获硬链接变更的 Windows 最小机制并证明与 SQLite 兼容，再将域权接入 Store 全入口；不能用上述测试源码或采样封门冒充保证。保留未提交状态，不操作主仓 index。

## 2026-11-01 SQLite 前置机制静态候选（待经理集中审查）

同一原 C35，HEAD/两 tree 不变，未提交；`execution_authority.rs` SHA256 `B238CCCE10A25DDA5C4CF9ECE98D45788ECD8660A43144F761727D12773F6EC7`，30699 字节。Cargo.toml/lib.rs 保留接收时注册/feature 增量。候选仅 OS/SQLite 前置机制，未接 Store，不是 NEXT-21U 完成或生产保护。

普通 NTFS 固定盘：逐级无 reparse 且不共享 DELETE 的目录句柄、instance.lock bootstrap、读写且不共享 DELETE 的 DB pin、文件 ID 命名 mutex、单链接和 canonical DB 路径。SQLite 仅从 pin 的名称打开；文件名复核外，通过 `SQLITE_FCNTL_WIN32_GET_HANDLE` 借用 SQLite main 句柄，只用 GetFileInformationByHandle 对比卷号/文件 ID/链接数，不关闭此句柄。初次创建后在取得 mutex 和单链接校验前不得执行 SQLite；错误 seal。实际由 Store 每次连接和事务准入接线仍未实施；本范围保护本产品连接，不宣称抵御同权限任意 raw IO。

本地 `libsqlite3-sys-0.35.0/sqlite3/sqlite3.h` 定义 GET_HANDLE 为 29，bundled `sqlite3.c` winFileControl 对该操作写出 `pFile->h`，bindings 已暴露对应常量；VFS open 的 share 为 READ|WRITE。上述是源码事实，非编译/运行证据。no-delete pin 阻止持有期间 alias 删除或 rename、预持 DELETE 阻止 pin、包含 POSIX disposition 的结论为待验证推断；微软 FILE_DISPOSITION_INFORMATION_EX 文档规定 DELETE disposition 须 DELETE access。

测试源码：`delete_sharing_and_disposition_controls` 使用真实 CreateHardLinkW、DeleteFileW、rename、预持 DELETE 和普通/POSIX disposition 独立正控；`domain_lock_covers_second_directory_and_late_hard_link` 改为 pin 存活期间删除应失败；`fresh_database_competitors_have_one_owner`、`directory_reparse_is_rejected` 为首次竞争/reparse，无法创建 reparse 即失败而非跳过；`uncheckpointed_wal_crash_rejects_alias_and_recovers_original` 用 ignored child、显式 --ignored --exact、缺环境报错和 RAII kill/wait，强杀后检查 WAL 并在删除别名后读回。进程状态/kill/wait 未能确定时不能继续认定安全；析构在异常路径输出 unknown，动态时必须首败停并核实进程终态。尚需经理审收异常收割机制是否足够，不能声称 OS 回收已证明。

有限组件窗口候选命令（仅经理放行后，在 Rust 工作目录且每次独立串行运行；先静审，失败/unknown 停止）：

```powershell
cargo test -p peachsh --lib execution_authority::tests::delete_sharing_and_disposition_controls -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::domain_lock_covers_second_directory_and_late_hard_link -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::fresh_database_competitors_have_one_owner -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::directory_reparse_is_rejected -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::uncheckpointed_wal_crash_rejects_alias_and_recovers_original -- --exact --nocapture
```

测试 helper 是 ignored，仅父测试显式执行；不得单独以缺环境的空结果当通过。候选还需要验证短名/大小写/相对路径与替换、同域多连接和实际 Store 别名；当前不是完整直接验收。Store 准备已核：main 当前 --check 跳过 instance.lock，Store::open 立即 WAL/schema 副作用；impl Store 分布在 budget、network 等模块，后续需逐入口事务 fence。provider_transport 返回裸 reqwest::Response，后续能力须贯穿 body/stream 结算。未运行 cargo、测试或进程；仅 rustfmt --check 和 tracked diff --check 成功，未跟踪文件由 rustfmt 单独检查。无主仓 index 操作。

### 入口接线准备增量

经理追加的 `plugin_management.rs`、必要时 `host_configuration.rs` / `project_config.rs` 已纳入授权。只读核到除 store.rs 外，`impl Store` 分散于 budget.rs、network_authorization.rs、host_configuration.rs、plugin_management.rs、project_config.rs；还需审查 worktree_management.rs 等直接持 DB 事务入口，不以 `impl Store` 搜索当完整证明。plugin 的 `admit_runtime`、`set_plugin_desired`、`set_runtime_desired` 均各自 IMMEDIATE，必须在各自实际事务校验，含幂等重放路径。配置写入按实际作用分类，不能一律改变权限。

Provider `provider_transport.rs` send 返回裸 Response；`provider.rs` 的 JSON 和流式路径及 `provider_runtime.rs` 两分支各自消费 bytes_stream，能力须跨 send、响应体和流处理实际生命周期，future 被 drop 不是底层工作完成证明。此为静态入口盘点，尚未实现/验证 fence。候选源码未因本盘点改变；一条 PowerShell 花括号通配搜索语法失败，已改为从 src 递归 `rg` 核对。

## 2026-11-01 U35-R01/R02 修订产品固定提交（待审、未动态）

按经理针对 B238CCCE 的两项阻断修订，固定独立树提交 `e08a2c5d1c65568e2370229f02c4742b4722872a`，产品 tree `1171ff68a835a25b39379170d9cfc867f697fa4c`、Rust tree `7df34d6723088e3d6df715c2bc454217ba19612c`，包含且仅包含授权的 Cargo.toml、lib.rs、execution_authority.rs；提交后 status/index 空。`execution_authority.rs` SHA256 `DF5FB10B79FEF24B5A6E2D9A61497626B66AD3BF7FD1D581B985D3DD44F51090`。原先 B238 单文件 hash 与命令仅为历史快照，本节取代其中的当前候选和 crash 命令。

R01：`PinnedDomain::acquire` 返回 Arc，`open_connection` 返回私有连接包装，包装按字段顺序先析构 SQLite Connection 再析构域 Arc，不提供裸连接所有权提取；同一 Arc 可创建多个连接。新增 `connections_keep_the_domain_pinned_until_the_last_one_drops`：两个 SQLite 连接持域，释放外部 owner 后以及释放第一个连接后竞争均失败，最后一个释放后重取并读数据为正控。尚未接 Store，不能声称全产品已经受保护。

R02：父测试本身标记 ignored；helper 同样 ignored，父方 explicit exact/ignored 且缺环境报错。清理使用原 Child 进程 handle 的 WaitForSingleObject、初始非 signaled 时尝试 kill、最多 5000ms 等待，确认 signaled 后才 try_wait；失败将原 Child 泄留保住 handle 并报告 unknown，非 panic 析构失败则触发失败，unwind 期间不二次 panic。不得用 unbounded wait；失败或 unknown 时窗口停止、保留现场，不能启动后续用例。源码静态自审，未证明实际强杀/收割。

经理静审后若另行签发有限串行窗口，在 `NoManCode/rust-app`，每条独立运行、首败或 unknown 停止：

```powershell
cargo test -p peachsh --lib execution_authority::tests::same_file_and_hard_link_contend_for_one_mutex -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::delete_sharing_and_disposition_controls -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::domain_lock_covers_second_directory_and_late_hard_link -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::fresh_database_competitors_have_one_owner -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::directory_reparse_is_rejected -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::connections_keep_the_domain_pinned_until_the_last_one_drops -- --exact --nocapture
cargo test -p peachsh --lib execution_authority::tests::uncheckpointed_wal_crash_rejects_alias_and_recovers_original -- --ignored --exact --nocapture
```

已运行 rustfmt 和 rustfmt --check、暂存完整三文件 git diff --cached --check，均退出 0；未运行 Cargo、测试或新进程探针。新增 wrapper 仍是机制前置范围，未来 Store 接线需提供不泄漏连接的受控事务接口并完整 fence；schema18、Engine/recover、Provider/工具 in-flight 未实施。下一步第一操作等待经理复审和精确有限窗口，继续按授权准备其余入口，不能当作 NEXT-21U 通过。

## 2026-11-01 U35-PRE-A attempt1 首步编译失败，窗口已归还

受测固定提交 `e08a2c5d1c65568e2370229f02c4742b4722872a`、产品 `1171ff68a835a25b39379170d9cfc867f697fa4c`、Rust `7df34d6723088e3d6df715c2bc454217ba19612c`。编译前后 HEAD/两 tree 一致、status/index 空、源文件 SHA256 `DF5FB10B79FEF24B5A6E2D9A61497626B66AD3BF7FD1D581B985D3DD44F51090` 一致；后核 UTC `2026-11-01T15:27:00.3120536Z`。整个 attempt 不改产品，编译失败后立即停止；六项均未启动，计数各未执行，WAL 父/helper 和全量 suite 均未执行。

Rust cwd，`cargo test -p peachsh --lib --no-run --offline --locked`，UTC `2026-11-01T15:25:48.1453792Z` 至 `2026-11-01T15:26:43.6011533Z`，cargo exit 101。唯一报告的编译错误 E0282：execution_authority.rs:310 的 `let mut handle = std::ptr::null_mut()` 无法推断 pointee 类型；并无测试结果。工具 cargo/rustc 1.98.1，运行 shim 位于用户既有 .cargo/bin；MSVC 14.43.34808 Hostx86/x64、SDK 10.0.22621.0 均只读既有安装。独立 `../nhc-c-domain-authority/.local/u35-pre-a-attempt1/` 保存 compile.log、compile-meta.json、compile.ps1、target、tmp；TMP/TEMP/target/log 均此 E 盘目录，jobs=2、dev/test debug=0、incremental=0 仅进程环境。

证据 SHA256：compile.log `5A2C2CA550696623B7E1D8AECFA2E1A9F7AB0313DCFC653BA94D14BA0F64B22F`；compile-meta.json `5D497F0782C54AE7307BBFF2242C2AC89EC60485DE7FE424A6FDC5AF2D0934B7`；compile.ps1 `9560BC7ED11776FFC012C2B95F1B41B5C51C3B55993FE927EE9A6F1CEF7A23B5`。未生成/运行固定测试二进制，故无测试二进制 hash。

停窗后仅将该变量标为 `windows_sys::Win32::Foundation::HANDLE`，rustfmt --check、diff --check 成功，固定新产品提交 `36ae19735f97387a42c2d19043d69d065cf8191e`，产品 tree `9cdb5e4f371b87d5f05f43c79ef5a4a140e4dccf`、Rust tree `2eb60f018a415201b75e83cee72f3d2f45aaab70`，源 SHA256 `31667263826BDFE62B877154CABB86DCEF3A66014915658BA2D78DF2A523719F`；新候选 status/index 空。未对新提交编译或重跑，需经理复审并另签窗口。WAL 异常保留设计仍待改：父退出会关闭泄留 handle，tempdir 清理可能丢现场；此次六项未结束，按指令不在窗口中改动或测试 WAL。NEXT-21U 未完成。

## 2026-11-01 U35-PRE-A attempt2 第二例失败，窗口归还

受测 `36ae19735f97387a42c2d19043d69d065cf8191e`、产品 `9cdb5e4f371b87d5f05f43c79ef5a4a140e4dccf`、Rust `2eb60f018a415201b75e83cee72f3d2f45aaab70`；前后 status/index 空、source SHA256 `31667263826BDFE62B877154CABB86DCEF3A66014915658BA2D78DF2A523719F` 一致，后核 UTC `2026-11-01T15:30:04.7981635Z`。未修改源码。独立 attempt2 复用 attempt1 target 缓存，TMP/TEMP 和本轮日志在 attempt2；编译 `cargo test -p peachsh --lib --no-run --offline --locked` UTC 15:28:59.7323348Z 至 15:29:23.6704681Z exit 0，3 条 warning 保留。工具与 MSVC/SDK、jobs2、debug0、incremental0 同 attempt1，非全新 target。

编译输出真实 exe 复制到 attempt2/peachsh-libtest.exe，原件与副本运行前后 SHA256 均 `BE74C64AD5A8908E86CABB8CA1F39A045CD833AFCBCC2DC9F1DF0AEF71F9BB6B`，绑定上述 source/编译日志。以该副本顺序调用完整 `execution_authority::tests::<name> --exact --nocapture --test-threads=1`：

1. `same_file_and_hard_link_contend_for_one_mutex`，UTC 15:29:55.6815245Z—15:29:56.1649416Z，exit 0，1 passed/0 failed/0 ignored。
2. `delete_sharing_and_disposition_controls`，UTC 15:29:56.1803661Z—15:29:56.2217804Z，exit 101，0 passed/1 failed/0 ignored。`CreateHardLinkW` 断言已通过，随后持有 no-delete DB pin 时 `DeleteFileW(alias)` 返回 1（成功），与测试期待 0 相反，断言在 execution_authority.rs:556 停止。先前预持 DELETE 阻止 pin 的断言已通过；该断言之后的 DELETE handle、rename 和 disposition 正控未执行。此单次观察否定本机本次“no-delete pin 阻止删除另一硬链接”的测试假设；不能推论其他平台、其他删除路径已验证，也不能凭采样把短暂硬链接封门称为保证。

后四个指定 exact 均未启动；WAL 父/helper、其他测试和全量均未执行。原始证据保留在 `../nhc-c-domain-authority/.local/u35-pre-a-attempt2/`：compile.log `CF69BC62B78D535D99F48C0C03DB376FEBAA512B549615564053491CE929BEE5`；compile-meta.json `AC84E8A484F844DBE3F80FBF702DCA08E0558C2B07A4F95868411CFF0A211DCA`；test-meta.json `DDBCAF14FB183D8EF77F28C488FAE5FC2FA16DDCEF8EE063E491375D61E2`；首例日志 `B50D4C7881DBAEE2AE2233F2AF1373323830E9D36B3DEA60EFD2A2A30DB7D9E3`；失败日志 `0C24166D13EF4316B720B9118042ADCBBF295B5FBCDD9B6AF1CA3E1559CD4F88`。逐例命令与 UTC 另见 test-meta.json；首败即退出，无边改边跑。

阻断不是简单编译问题：现有离散 link-count 检查可能漏掉创建并删除别名，机制前提不成立；不能仅改断言。窗口已归还，等待经理处理此真实反例和后续最小机制取舍；六项未通过，WAL 活父保留/Store/epoch 未继续，NEXT-21U 未完成。

## 2026-11-01 活 owner 别名与 owner 死后路径绑定静态增量

经理限定保护本产品入口及连接，不视同权限任意 raw IO 为本任务 OS 沙箱。固定 `36ae19735f97387a42c2d19043d69d065cf8191e` 基础上，仅新增 `pinned_original_and_transient_alias_cannot_form_a_second_product_domain` 测试源码：在原路径 owner 与 SQLite 连接存活时，尝试删除/重命名选中原路径应被拒；创建别名后从第二目录 acquire 须因同 file-ID mutex busy，检查别名侧无 WAL/SHM；删除短暂别名后原连接读正控，释放 owner/连接后原路径 rename 与恢复重开为正控。准备失败或原名删除成功均为失败，不跳过。此测试尚未编译/运行；同目录 instance.lock 文件创建并不代表 SQLite 已打开。旧 `delete_sharing_and_disposition_controls` 和 `domain_lock_covers_second_directory_and_late_hard_link` 的错误假设与历史失败源码均原样保留，不能混为已通过。rustfmt --check 与 git diff --check 0；源文件未提交 SHA256 `6269BF530B85E738BCDDFAC49F49026953F013ED17219B066F4AB21242DDC85B`，index 空。未执行任何测试、动态或进程。

静态顺序：acquire 先目录 pin、bootstrap，再打开 DB pin、取 file-ID mutex，最后校验/形成 owner；仅 owner 的 Arc 可调用 open_connection，固定同一 canonical 名并用 SQLite GET_HANDLE 核文件对象。若所有产品连接真正都走此路径，且选中原路径 pin 的删除/替换约束得到验证，活 owner 期间短暂别名先遇同文件 mutex，不能通过本机制 acquire 后打开 SQLite；别名删除不改变此同 owner 的原 sidecar。当前 Store::open 等尚未接线，不能将该推理写成现产品保证。连接在途若引用释放、绕过 wrapper、或域检查与实际副作用之间未受 fence 保护，仍可能破坏完整执行权。

具体跨死亡反例：A 以原目录 P 打开 DB、提交未 checkpoint WAL 后异常退出，锁和 pin 均释放；外部删除 P 的主 DB 链接，只留另一目录 Q 的硬链接，或者把主 DB 从 P 移动到 Q，留下 P 的 WAL/SHM。Q 此时单链接，同文件 ID mutex 无持有者、Q 的 bootstrap 空闲，现有 acquire 会接受 Q 并以 Q 的不同 sidecar 名称打开 SQLite；文件 ID、单链接与 SQLite handle 身份全相等，仍不能找回 P-WAL。P 端原文件缺失时目前 create(true) 还可能创建新的空 DB。未经持久路径绑定无法从 SQLite 打开后补救：一旦打开，WAL 恢复/副作用可能先发生。owner 存活时路径 pin 防护本身亦须动态证明，不据失败例推断它已成立。

建议待经理裁决的最小方向：在首次允许 SQLite 打开之前，把规范化实际主 DB/sidecar 域名与文件身份写入跟随 DB 对象的持久元数据，例如 NTFS 命名数据流（ADS），并 flush；下一 owner 在持 DB pin、file-ID mutex 后、SQLite open 前读该元数据，缺失/损坏/不一致一律封门，不能将旧对象自动重新绑定到 Q。新文件需可区分地进行专门 bootstrap：排他创建、保存可恢复创建意图、持久绑定后才允许 SQLite；崩溃不确定状态封门，禁止把不确定旧库当新库。元数据的原子写/刷盘、路径规范与大小写、ADS/备份丢失、首次建库中断、其他卷及复制都须有明确验收。ADS 随硬链接及文件重命名关联这一 OS 语义也须经平台文档和受控测试验证。备份/复制可能不携带 ADS，应明确拒绝并走受控导入/迁移，不静默采纳新目录。原有未绑定 schema17 数据库需要一次显式、可审计的受控接纳；如有残留 WAL 必须保持原 DB、WAL 名及目录不变，不能证明来源时封门。该外部预绑定须先于任何 schema18 迁移，schema18 可另记录 domain/epoch/phase，但不能用数据库内表取代首次打开前的绑定。该方案会改变既有库接纳、移动和备份恢复兼容性，未经经理裁决未实现；无本轮动态结论。WAL 异常活父保现场仍待另行静态修订。

## 2026-11-01 ADS/初始化静态实施中（尚非完整候选）

基于 `36ae19735f97387a42c2d19043d69d065cf8191e`，本轮未提交的 execution_authority.rs SHA256 `60207CD83B37269251C4221F2C39DDF6FE72865830C4B9D77FCAE2586B225CC3`；index 空。新增目录 bootstrap marker 先写/flush，再 create_new DB，已有 marker 却缺 DB 及缺库但存在 WAL/SHM/journal 拒绝；现存未绑定库给 typed `LegacyDomainRequiresControlledAdoption`。取得 DB pin 和 file-ID mutex 后、SQLite 打开前校验 ADS（有界、固定版本、卷号/文件 ID、GetFinalPathNameByHandleW volume-GUID 规范完整路径，字节精确匹配，任何不一致失败），首次创建 ADS 后 flush，打开 ADS 时校验与 DB 相同文件身份；后续 verify 再检查 ADS。选择更严格完整路径：大小写/短名依赖 OS normalized handle 返回值，差异安全拒绝；目录整体搬迁亦拒绝，单 DB 搬迁拒绝，跨卷/复制需受控导入。目录路径固定、真实 SQLite handle 和所有连接接线仍须验证，不能声称已运行。

新增 `adopt_legacy` 受控接口草稿：只有拿到私有 `LegacyAdoptionProof`（绑定文件身份和路径）才能写 ADS、flush、再写 marker；没有可用于生产的证明签发入口，测试专用 fixture proof 只用于模拟受信路径，不能当作真实来源证明，更不能向用户库执行。完整产品接线前仍需确定可信证明签发、用户可审的原 DB+WAL 来源和失败恢复；不能由任意输入路径自签、不能强制清理或改变 unknown。ADS/marker 缺失/损坏/截断、marker 后缺库或中断、单文件移动、整目录移动拒绝、只剩 alias、临时 alias 竞争、持续持权与 legacy live-WAL 夹具均有静态测试源码；旧失败断言保留并标记 ignored，历史日志未改。未实现完整 WAL 强杀异常活父/监督 runner，现有 Reap 在 unknown 时丢父对象/现场问题仍不能用于正式 crash 窗口；真实 WAL 崩溃恢复测试尚未实证。

已完成 rustfmt --check、git diff --check；未编译、未运行测试/进程。此为进行中草稿，不是可固定交审的完整 ADS/初始化候选，不可声称功能完成；还需自审 ADS 文件句柄/路径、创建顺序与可恢复状态、受控接纳可信证明、WAL supervision 等并形成完整候选。schema18 owner/epoch/phase 和 Store/Engine 全入口仍未接线。

两轮 PRE-A 证据已逐字节发布至 `niuma/员工C/提交报告/第三十五轮/U35-PRE-A证据/`：原始日志/meta/实际脚本共 10 份；manifest.json 给出源与发布相对路径、逐份字节数和双 SHA256、两次 source/双 tree。所有原/发布 hash 一致；target 与 38,805,504 字节测试 exe 未复制，exe 原位置和 hash 记录在 manifest，原件未改删。日志内部历史绝对路径保持原始字节。主仓 index 未操作。

## 2026-11-01 ADS/初始化前置机制固定静态候选

上一节的进行中 hash 已过时。现固定独立树提交 `6f9217518713a28e9bc14cf9927bbbbf13cff3e7`、产品 tree `890996a316820c562e709ef2348f942b92707d37`、Rust tree `7e84b214bf68cab7faf91b3be3c3586f5af317e5`，提交后 status/index 空。包含 execution_authority.rs 与专用 `tests/support/execution_authority/run-wal-case.ps1`；文件 SHA256 分别 `63CEE3A0F46E074FBD65B351449ED609A901EABCFF835FB05A4843DD07C1AB07` 和 `D36E519D594C0D3D4B02A76533EE4CF16FAA3E5DA4F11292212CA03CD06D92FF`。固定的是待静审的前置机制，不是 NEXT-21U 产品接线或验收。

严格采用已打开 DB handle 的 normalized volume-GUID 完整路径及卷/文件 ID 作为 ADS 内容。该严格策略拒绝整目录搬迁，包括同一父目录对象整体改名；普通不同盘符指向同一卷若 OS 返回相同 GUID 名可能统一，大小写/短名以 OS 返回的 normalized 名为准，否则安全拒绝。DB 与 sidecar 固定原目录及 basename；发现多链接/身份/ADS 不符封门，不声称检测曾出现后消失的 alias。本机制只覆盖通过本产品受控域入口的连接，不作为抵御同权限任意 raw IO 的沙箱。

bootstrap 在锁下先创建/flush 目录意图，再 create_new 数据库、取得 file-ID mutex、创建并 flush ADS，校验后才可 SQLite open。已有 DB 无目录标记或 ADS 给 typed legacy 诊断；已标记缺 DB 或缺 DB 却有 sidecar 直接拒绝。流读取限 16385 字节，与规范化预期的版本/身份/路径逐字节比较，缺失、截断、篡改、错域拒绝；流 handle 与 DB file-ID 相等方接纳。发生中断或不确定不自动重绑；关于 NTFS ADS、flush/目录元数据持久性及实际句柄身份均尚待动态证据和经理静审。

`adopt_legacy` 为内部受控接口：仅接受私有 `LegacyAdoptionProof`，测试 fixture 可构造，生产没有签发入口。它在域锁/文件 ID mutex 下校验证明与对象、写并 flush ADS，再写/flush marker，不读写 SQLite 或重派 unknown。此接口的真实来源审核、DB/WAL 完整性证明及用户可审接纳入口尚未实现；目前不能接纳用户真实旧库，也不能把测试 fixture 证明当生产证明。未来接口必须确认原域及 WAL 的可信来源并保留 legacy schema17 内容；表内 schema18 不能替代打开前 ADS 判定。

新增源码覆盖绑定缺失/损坏/截断和已观察失配封门、原路径替换、临时 alias 竞争、sole alias、DB 单独移动、目录整体移动严格拒绝、bootstrap 中断/不重生、legacy fixture 与 live WAL 内容保持、正常连接及多连接保权。既有被 attempt2 实证否定的两个假设用例仅标 ignored，保留原断言与失败原件；未以修改断言粉饰。新断言均未编译或运行。crash-WAL 父/helper 保持 ignored；unknown 收割时父进程持 Child handle 不退出、写 unknown 并保持 case 目录，监督脚本超时/unknown 返回 2 而不杀父进程，case 和日志保留；故意保留存活对象须经理批准的诊断流程后处理，不能视为成功回收。runner 尚未运行，需审证通知失败和超时分支，不得在 B 窗口启动。

执行证据归档为 [U35-PRE-A 证据清单](U35-PRE-A证据/manifest.json)，10 份原件/副本逐字节 hash 相同，target 和测试 exe 不复制。静态验证：rustfmt --edition 2024 --check、暂存全部候选 git diff --cached --check、PowerShell Parser 语法检查均 0；没有 Cargo 编译、测试或进程探针。真实受控接纳签发、公开兼容、schema18、Store/Engine/recover 与 Provider/工具在途接线均未完成。

## 2026-11-01 U35-R03 父目录对象绑定静态修订

经理指出原绑定仅含 DB ID 与 GUID 完整路径，不能识别同路径父目录对象替换。修订单独固定 `09c690d48efdea8da17552ae018fbf1b0f9eaff8`、产品 tree `667dd934a2d9f9f55beb1348932c9292b6c51730`、Rust tree `a7fe0165e9f2cece8eac3483537b1b48e1bfc932`，source SHA256 `BE3E0CB89D507E290E3B6E258DD59C2D15EC99DB945CF7AEB5AF54E3071831E0`，提交后 clean/index 空。仅 rustfmt/check、diff check，未编译/测试。

新增独立 DirectoryIdentity，保活目录句柄，取父目录卷号/文件 ID；ADS 版本升为 2，绑定 DB ID、parent ID、固定 basename 与严格 GUID 完整路径。每次校验对比保活父句柄与当前路径重开父目录对象；legacy proof 也包含 parent。相对路径的 `..` 同步弹出对应目录 pin，确保最后的 pin 是实际父对象。旧版本 ADS 不自动接纳。新增测试：旧 P 移 Q 后新建 P、将原 DB/marker 移回 P 且 sidecar 留 Q，须以 binding mismatch 拒绝，再恢复原父对象及正常读为正控；另测旧 legacy proof 不得跨同路径父对象替换。未运行，不能将测试源码称为通过。

此提交可供经理基础组件静审与后续安排，WAL runner 的来源/创建时间/unknown 闩锁等 R04 仍待独立静态修订；Store/schema18/Engine/在途与真实旧库接纳未完成。

## 2026-11-01 U35-R04 WAL 监督源码静态修订

在 R03 `09c690d4` 后独立提交 `559e337feb503c73571d80e869a7b101c73b7a45` 与异常登记补充 `a799abc1eb10c7ee42bbcc39e227ff3bfb4d56ac`；当前完整 HEAD `a799abc1eb10c7ee42bbcc39e227ff3bfb4d56ac`，产品 tree `c0d65c90a3621cf9132c95a979076f3c75c8178e`，Rust tree `1c9715a687d2da1fc1eee0427496e1f525e6a1c9`，status/index 空。仅修改 C 自己的 execution_authority support；DirectProcess.cs 以 B 的只读同类文件为基础复制并改为 C 独立类、增加原 handle 等待及退出码读取，未修改 B 树。

监督器要求固定 commit、二进制与构建回执：回执 JSON 需包含 `head`、`product_tree`、`rust_tree`、成功 `exit_code` 和 `test_binary_sha256`；对照当前 clean HEAD/双 tree，复制并前后核原件与固定副本 SHA256、记录长度、回执自身 hash 和来源路径。回执是构建证据的关联，不自动证明构建过程；实际窗口必须保存可核的构建命令/日志。调用 C 独立 DirectProcess 使用 CREATE_SUSPENDED/NO_WINDOW、直接可继承文件句柄承载 stdout/stderr（无监督者管道读线程），从原进程 handle 取 PID/创建 FILETIME 并登记后 resume，使用该 handle 有界等待和取退出码。记录起止/状态、unknown 原因、case/incident、初末 source；异常或无法确认时不 terminate/猜杀活父，留下 `.local/nhc-u35-wal-window.lock` 跨 attempt 闩锁；runner crash 亦因 CreateNew 锁文件保守阻止新 attempt，需人工证据核查后处理。

测试父内部 unknown 时持原 Child handle、保留 case 目录并写 unknown 文件后驻留；监督器检测 unknown 或超时停止并保留父进程；stdout/stderr 直接文件，不依赖监督者存活。这是静态设计与实现，不是实测生存证明；进程创建/创建时间获取/写 incident 失败、监督器或测试父崩溃等均保持 unknown，不能推断子进程回收。监督器退出后自身的原父进程 handle 不再保活，活父才保有其 Child handle；异常状态须按 PID+创建时间及保留现场独立诊断，不自动重新打开窗口。最终 WAL 验收仍需真实受控验证，且当前无动态授权。

PowerShell Parser 与暂存 diff --check 0；C# 未编译、runner 未运行、无进程探针。runner SHA256 `ED0138A6F483920424CD8F21E19F26FC0D1C66C0FA3326E7192F575292771310`，DirectProcess.cs `7B40CFC51658757F87E16478026358F2535233FC80BE89D2BDD1A1AB5B6BFB9D`。R03 原独立提交仍可按 `09c690d4` 单独审阅；当前完整头含 R04 两提交，待经理集中审查与精确动态窗口。Store/schema18/真实接纳及执行全入口仍未完成。

## U35-PRE-B a799 组件与 WAL 窗口（2026-11-02 本地 / 2026-11-01 UTC）

受测 HEAD `a799abc1eb10c7ee42bbcc39e227ff3bfb4d56ac`、产品 `c0d65c90a3621cf9132c95a979076f3c75c8178e`、Rust `1c9715a687d2da1fc1eee0427496e1f525e6a1c9`，前后 clean/index 空、source SHA256 `BE3E0CB89D507E290E3B6E258DD59C2D15EC99DB945CF7AEB5AF54E3071831E0` 一致。E 盘独立 attempt2 风格新目录 `../nhc-c-domain-authority/.local/u35-pre-b-attempt1/`，target 复用 PRE-A attempt1，TMP/TEMP 新目录，Rust 1.98.1/MSVC 14.43.34808 Hostx86/x64/SDK 10.0.22621.0 只读既有安装，jobs2/debug0/incremental0。`cargo test -p peachsh --lib --no-run --offline --locked` UTC 2026-11-01 15:58:08.3989987—15:58:33.0468568，exit 0，3 warnings；原日志保留。生成 lib test 副本、构建目标及 WAL 副本均前后 SHA256 `72E468A6F7AEBD6240F81F437D7EA61863BD4BA52A3BF76EF070A78DFA7F5901`，38,901,760 字节；本轮 build receipt 与固定 commit/双 tree/source/log/二进制绑定。

以下完整名称均以 `execution_authority::tests::` 为前缀，均为独立固定二进制 `--exact --nocapture --test-threads=1`，UTC、exit 与真实结果如下（每项 1 passed / 0 failed / 0 ignored）：

| 名称 | UTC 开始—结束 | exit |
| --- | --- | --- |
| binding_missing_corrupt_and_truncated_are_closed | 11/01/2026 15:59:12 — 11/01/2026 15:59:13 | 0 |
| observed_binding_corruption_permanently_seals_owner | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| initialized_missing_db_and_bootstrap_interrupt_refuse_recreation | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| single_link_move_and_whole_directory_move_are_rejected | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| same_path_with_replaced_parent_rejects_bound_database_and_marker | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| legacy_proof_cannot_be_reused_after_same_path_parent_replacement | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| sole_surviving_hard_link_is_not_silently_adopted | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| legacy_requires_proof_and_adoption_preserves_live_wal | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| same_file_and_hard_link_contend_for_one_mutex | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| pinned_original_and_transient_alias_cannot_form_a_second_product_domain | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| fresh_database_competitors_have_one_owner | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| directory_reparse_is_rejected | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |
| connections_keep_the_domain_pinned_until_the_last_one_drops | 11/01/2026 15:59:13 — 11/01/2026 15:59:13 | 0 |

13 项均实际为 1/0/0；历史两个 ignored 反证未运行。其后仅运行监督脚本中一个 WAL 父 exact（`--ignored --exact`），父 case UTC 2026-11-01 15:59:30.2889096—15:59:31.1871199，exit 0，父测试 1 passed/0 failed/0 ignored；helper 仅由父启动、实际被强杀，不将其 `running 1 test` 计为通过。监督 case PID 77384、原对象 creation FILETIME 134380223706854534；case state passed、无 unknown，闩锁检查不存在。原 stdout/stderr、case/source JSON 保留，case 现场继续留 E；未独立运行 helper 或全量 suite。父由 DirectProcess `CREATE_NO_WINDOW` 启动，helper Command 本身未显式 NO_WINDOW；本轮没有可靠视觉观测，实际是否出现窗口未知，不声称已证实隐藏或可见。

原始 compile/test/WAL 日志、meta、执行脚本、runner 源及 C# 共 27 份逐字节发布 [U35-PRE-B 证据清单](U35-PRE-B证据/manifest.json)，原/发布 hash 一致；target/38,901,760 字节测试 exe 不复制，路径与 hash 记录清单。构建记录和每例完整命令/时间在 evidence/components 的 compile-meta.json/test-meta.json；WAL 原始元数据在 evidence/wal。窗口结束归还，B 可按经理调度；该结果仅限固定候选 ADS/前置测试，未验证真实旧库接纳、Store/schema18/Engine/在途全部接线，也未覆盖 WAL 监督 unknown 反例、helper 视觉隐藏或全量 workspace。无产品源码修改，主仓 index 未操作。

## 2026-11-02 PRE-B 后 NEXT-21U 静态接线进行中

经理限定核收 a799 OS/SQLite 前置 14 项，监督故障注入仍未验证。本轮继受测干净基线后，独立树 execution_authority.rs 未提交 SHA256 `C1221831A769431A877B9F7C988FC367AEBE0DF69B5B89D508CA5DAB3E60D462`；仅 rustfmt --check 与 git diff --check，未编译/测试/进程。WAL helper 新源码增加 CREATE_NO_WINDOW；PRE-B 当次 helper 是否出现窗口仍未知，不能追认为隐藏。

开始受域连接/owner 静态基础：schema17 后新增受控 IMMEDIATE schema18 owner 表初稿，含 domain_identity、非负 epoch、recovering/active、迁移 marker；登记/激活改接私有 PinnedConnection 并要求同一 Arc 域，防跨域连接；提交不确定及事务内校验失败封门；增加 schema18/epoch/跨域拒绝测试源码。此初稿调用方必须先验证完整 schema17，尚未接入 Store，未提供完整 schema18 重开验证和真实迁移回归；`activate` 尚未由不可伪造 recovery receipt 约束，不能认作完成恢复封门或开放运行入口。当前 Store::open/--check 仍为旧行为，Engine/所有散布事务/Provider body 与工具在途均尚未接线，真实 legacy 受控证明及入口未实现。此状态不是完整候选、不提交审定、不改变 a799 组件通过事实。

下一步：设计 Store 与 PinnedConnection 不暴露可替换裸 Connection 的内部事务持有方式，将验证及 schema18 路径接到迁移前取得的真实域；补完整 schema18 检验和恢复 receipt，再逐个事务 fence 与 in-flight 接线。任何新候选必须集中静审后另签动态窗口；PRE-B 原件/27 对发布证据保持不变。
