# B21-GATES-B attempt3 执行记录

2026-11-02 员工B。经理签 [attempt3 窗口](../../审查记录/第二十一轮/2026-11-02工具链显式入口与attempt3签窗.md)。启动前核 f898/双tree、clean/index、192源码和四份runner hash一致，无B路径匹配进程。UTC 2026-11-01 23:08:48 独占打开 attempt2 锁成功，核界及目的不存在后保全迁移，原位置 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/nhc-w2-window.lock`，新位置 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-gates-b-f898-attempt2/nhc-w2-window.original.lock`；前后 SHA256 `6262379b285922f249312805f1d134d2110bfadd8452b52945a25d99a8c41491`，原位置不存在。旧清单与原记录不改。

即将按签窗显式设原 MSVC/SDK 根并启动 attempt3，持续工具会话；首败或 unknown 停止。

## 实际结果：第五门首败停窗

原工具会话 15606 实际退出1。launcher 记录 supervisor 原句柄 PID 82924、creation FILETIME 134380481414299461，UTC 2026-11-01 23:18:56.6392793 signal、exit 1。各门均为 runner 原进程记录，UTC 时间如下：

| 门禁 | 开始 UTC | 结束 UTC | PID / creation FILETIME | 结果 |
| --- | --- | --- | --- | --- |
| workspace all-targets check | 23:09:03.2096483 | 23:09:03.9814562 | 81616 / 134380481432204944 | exit 0 |
| debug Host build | 23:09:04.7007660 | 23:09:14.5839651 | 82648 / 134380481447031053 | exit 0 |
| workspace test `--test-threads=1` | 23:09:15.4671130 | 23:18:39.4132472 | 6968 / 134380481554694669 | exit 0，563937ms |
| fmt check | 23:18:40.0891849 | 23:18:41.8855431 | 78988 / 134380487200909533 | exit 0 |
| workspace all-targets Clippy `-D warnings` | 23:18:42.5541962 | 23:18:56.1794797 | 66796 / 134380487225558271 | exit 101 |
| UI wasm check | — | — | — | 未启动 |

Clippy stderr 有5处诊断：`wasm_worker_process.rs` 1734、1751、1759 的 `CreateEventW` / `CreatePipe` security 参数，以及 `wasm_worker_process_tests.rs` 1125 的 `CreateEventW` attributes 参数，均为 `unnecessary_mut_passed`；同测试文件1543–1545在 `is_some` 后 `unwrap`，为 `unnecessary_unwrap`。这些是当前源码的 lint 失败；未修复、未重跑。21项受控矩阵未重复；串行 workspace test 的成功不能写成默认并发测试。

新锁保留在 `.local/nhc-w2-window.lock`，SHA256 `1913b9fa7a99ed3b0c7c5d60111308509f329d15014fd92a78a453b1b40757f4`。源码 before/stopped 均192项且逐项相同，HEAD/双tree和 clean/index 不变，当前B路径匹配进程快照为空。Host 独立副本最终 SHA256 `f6883f22a34acb49061dfae807ffcbfb9d2dac9d1728421b78749689e87aa59e` 与其构建时记录一致；不把可变target当前值当作该副本。无 source-final，停止快照为 source-stopped；不是六门通过或W2最终验收。

environment.json 实际记录 MSVC 14.43.34808、SDK 10.0.22621.0、E target/TMP、jobs2、debug0、incremental0、offline/locked 和单线程。build.ps1 根据已检查存在的 Hostx86/x64 三工具及 Hostx64 缺失状态选择 Hostx86/x64 是静态推断；环境记录未单列所选 `taskBin`，不冒称有实际路径回执。未安装工具链。

[原始证据](B21-GATES-B-attempt3原始证据/)为现场顶层47件与原锁副本，共48件、478558字节；源前后及副本双hash和长度复核零差异，不包含 EXE、target 或 tmp。[清单](B21-GATES-B-attempt3原始证据/双hash清单.json) SHA256 `37baf313be64fe5aad9b3cf35448b809647bd52c655a35f36472d80062cf807e`，原日志逐字节保留。下一步经理审首败、裁定锁与修复/验证范围；不自动重试。
