# B21-W2-FULL-A 首次停窗及 runner 最小修订

2026-11-01，员工 B。经理签唯一窗口，受测 `e544db0cc82433512e4bcb23345202e8ab22fa5c`，产品 `2e0ac6142f503bf53f35da729bb4c0cecaf3ffc9`、Rust `dcd55e88295babe42fee0e28c2055def2ce268c7`，受测前后 HEAD/tree 相同、工作树/index 空。UTC 15:36:10.4905480 至 15:36:51.8364627。现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-a-e544-attempt1/`。

入口 `run-controlled.ps1 -ExpectedHead e544db0cc82433512e4bcb23345202e8ab22fa5c -TargetDir <B旧独立target> -AttemptDir <本轮新runner> -CaseTimeoutSeconds 180`；真实参数与 E 盘路径在现场 `environment.json`、`start-window.ps1`。TMP/TEMP、target、输出在 E，offline/locked、jobs2、dev/test debug0、incremental0，项目现有 MSVC 14.43.34808 和 SDK 10.0.22621.0。外层 PowerShell 隐藏启动，观测 MainWindowHandle=0；受测 DirectProcess 实际代码 flags 仅 `0x00000004` CREATE_SUSPENDED，未声明其附加 CREATE_NO_WINDOW。未独立覆盖每个瞬时窗口可见性。

Add-Type 成功；worker/probe build、lib no-run 分别完成且 runner 越过各自非零检查（各自原始退出码未单独落盘，按控制流为 0），固定三件后进入 `__list__`。清单进程输出 23 个测试名（20 受控 exact 加 3 helper），但在 runner 执行映射校验前，空 stdout/stderr 相加产生 null，`run-controlled.ps1:75` 调用 `.Contains` 失败，runner 约 489ms 标记 terminal_unknown 并以窗口 exit 1 停止。清单进程退出码未取得，不称 list 通过；20 个 exact、所有测试函数及其 helper、fmt/Clippy/回归均未运行。

固定源与副本前后 SHA-256 一致：worker `2fc364dfe8ad85d709a8deced561985c4a2b7db92231d5c17ff806c5c2c49c5c`；probe `cacd1822ab43ed45f587daf95d5ac0936a47dce083e7902f5342f1453c5a9ed2`；lib-test `719a29347a8853d60d88ff1e676a79239ec31022d86aa6756b6242ffa1318bfe`。清单及事后源/固定件重算见原 `runner/manifest.json`、`runner/hashes-after.json`；没有执行按例 probe 副本准备。

事故 `runner/incident.json`、`runner/listing/case.json`：`__list__`，nonce `9e63486a92cd4f07896001b0d40c848f`，PID 76832、creation FILETIME `134380210110999580`。UTC 15:38:54.9352503Z 只读 `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE)` 失败 error 87；不能获得原句柄，故未观察到原对象 signal/退出码。同期 PID/父子查询与相关测试可执行文件进程快照空。结合 runner 记录的 list-only 参数和 23 行列举、未进入任何 exact，限定判断该枚举进程现在已不在运行且本次未启动测试子进程；不将当前 PID 不存在追认为此前持原句柄观察到退出。

经理授权下将原 lock 原样移动至 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-a-e544-attempt1-audit/list-terminal-gate.original.json`，前后 SHA-256 `359e5f39ea652f3dd219f8ad08b48816bc64389973e1ed92d95d5ead69e76024`；gate 与 incident 身份/nonce/原因一致，JSON 编码字节不同，分别保留。`process-audit.json`、`pid-and-children-snapshot.json`、`worker-process-snapshot.json`、`gate-archive.json`、`original-files.json` 及复核在 audit 目录。22 份首次现场文件散列复查均不变。没有删除、重跑或按 PID 杀进程。

修订分两次固定：`5d4358452fee1d1f7f094252a88af9b4c961d43f` 将 stdout/stderr 和 stage 空值以 `String.Concat` 归一为字符串；随后 `249479ad1f42263c12dccf7d5d4b467c1331319f` 将 stage 的 Concat 固定双参数并把直接启动 flags 改为 `0x08000004`（CREATE_SUSPENDED | CREATE_NO_WINDOW）。最终产品 tree `7889e6e7c2686c47dcb1c7a7a9a167cfa11756c5`、Rust tree `fb61c035b923b2d883c760efb86489c7509a01e4`，独立树 clean/index 空。相对 e544 仅 `run-controlled.ps1` 和 `DirectProcess.cs` 两个支持文件变化。PowerShell parser、git diff --check 通过；修订未编译或运行，新的无控制台效果也未动态验证。窗口已归还，下一步经理 review 固定增量并决定新窗口；本次不是 W2 验收或 main 合入。
