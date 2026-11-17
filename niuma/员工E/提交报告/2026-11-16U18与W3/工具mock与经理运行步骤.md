# E 工具 mock 与经理运行步骤

2026-11-16，E 树 `733f420bc6e09234102af637ba8fd833321fdced`。本次所有 E 负责的工具 mock 均用 Python `-B` 重跑，未启动 Cargo、W2 测试、产品进程、真实 probe 或安装。日志相对本仓库：

|工具及命令|结果|本树保留日志|
|---|---:|---|
|`python -B NoManCode/rust-app/tools/worker-package/test_worker_package.py`|22/22|`.local/u18-e-evidence/final-worker-package-mock.log`|
|`python -B NoManCode/rust-app/tools/worker-package/test_bootstrap_runner.py`|2/2|`.local/u18-e-evidence/final-bootstrap-runner-mock.log`|
|`python -B NoManCode/rust-app/tools/lifecycle/test_lifecycle_gates.py`|25/25|`.local/u18-e-evidence/final-lifecycle-mock.log`|
|`python -B NoManCode/rust-app/tools/lifecycle/test_w2_controlled.py`|4/4|`.local/u18-e-evidence/final-w2-controlled-mock.log`|

日志为 E 工作树本地证据，不随 Git 提交；不要把纯 mock 当真实进程结果。此前 `ed0a6f24` 对生命周期等待后检查 deadline；`90bbde40` 为 W2 单门禁入口；`733f420b` 加入两个 W07 shutdown 测试，映射共 27 项。D 已复核 `733f420b` 工具源码：完整 listing、五个 stage unknown、Cargo JSON 工件选择、复制/hash 和独立 case 在静态审中无新增问题，未执行真实验证。用户报告 full-discovery 普通失败且无 terminal_unknown、原窗口已收束；这是用户提供的窗口状态，E 未自行执行或核实其日志。

D 已固定 shutdown 与 Host 取消接线提交 `bb982457a378a5ad234127daeddf50cde3a9f6d4`，依赖其前序 `239f3921` 及 A/B 接口；E 只读核对 D 源文件的六个 ignored W07 名称均与 `733f420b` 匹配。最终编译、listing 与运行须以经理组合后的源码和证据为准。

经理组合后先以组合的完整 HEAD、`HEAD:NoManCode`、`HEAD:NoManCode/rust-app` 三个 ID 替换以下变量；不可沿用先前组合 ID。须先纳入 `ed0a6f24`、W2 入口和 D 最终全部 W07，完成组合编译、在唯一窗口下执行。下列命令从组合仓库根运行；实例名称必须未被使用。W2、pair、bootstrap/Host 矩阵各自独占同一 common-dir 门禁，当前 runner 完成后会保留该门禁；经理须审查本次对象与证据并按项目规则收束，才能开始下一次。输出 target、临时及证据目录都在 E；禁止对 PID 猜杀、删除未知现场或自动解锁。

```powershell
$headId = '<组合完整HEAD>'
$productTree = '<组合NoManCode tree>'
$rustTree = '<组合rust-app tree>'

# W2 全部27项；单项可将 --all 换成 --case <完整测试名>，可重复 --case。
python -B NoManCode/rust-app/tools/lifecycle/w2_controlled.py `
  --head $headId --product-tree $productTree --rust-tree $rustTree `
  --attempt w2-combined-attempt1 --step-timeout-seconds 1800 --all

# 独立窗口：新建 debug pair，不安装或切换 current。
pwsh -NoProfile -File NoManCode/rust-app/tools/worker-package/package-worker.ps1 `
  -Head $headId -ProductTree $productTree -RustTree $rustTree `
  -Pair u18-candidate-pair1 -Profile debug -StepTimeoutSeconds 1800

# 独立窗口：生产 bootstrap 的缺摘要、格式/哈希/混搭及 pin 测试。
pwsh -NoProfile -File NoManCode/rust-app/tools/lifecycle/run-gates.ps1 `
  -Head $headId -ProductTree $productTree -RustTree $rustTree `
  -Attempt u18-bootstrap-attempt1 -Mode targeted -TestTarget worker_package_bootstrap `
  -Filter packaged_bootstrap_rejects_mismatch_and_pins_fixed_copy -StepTimeoutSeconds 1800

# 独立窗口：固定 candidate pair 和基线 foreign/unbound Host 的 --check 矩阵。
$env:NHC_TEST_PAIR_A = (Resolve-Path '.local/worker-packages/u18-candidate-pair1').Path
$env:NHC_TEST_FOREIGN_WORKER = (Resolve-Path '.local/u18-fixtures/baseline-binaries/peachsh-wasm-worker.exe').Path
$env:NHC_TEST_UNBOUND_HOST = (Resolve-Path '.local/u18-fixtures/baseline-binaries/peachsh.exe').Path
python -B NoManCode/rust-app/tools/worker-package/run-bootstrap-check.py `
  --head $headId --product-tree $productTree --rust-tree $rustTree `
  --attempt u18-real-pair-attempt1 --step-timeout-seconds 300
```

执行前检查 Python、PowerShell、离线 Cargo 依赖和已安装 MSVC/SDK；工具自动发现，必要时提供 `VCToolsInstallDir`、`VSINSTALLDIR`、`WindowsSdkDir`。基线 `peachsh.exe` SHA256 为 `46d3b09c8d7634c1ff7a280758c90cede7430196347f8753c04c8cc9bb525956`、foreign worker 为 `4d4a3dbea3edd9c9957bcd90dc30e9fbf5a9356fc0f2996a40ee4ed94c3d12b3`，来源记录 `.local/u18-baseline-selection.json`；经理以实际记录和文件核对。candidate worker 必须与 foreign 字节不同。

W2 查看 `.local/lifecycle-w2/w2-combined-attempt1/` 的 `manifest.json`、源码快照、固定工件 JSON、listing、每个 case 的日志与 stage；确认全量映射匹配、每项 exactly one、所有固定副本 hash 不变。pair 查看 `.local/worker-package-evidence/u18-candidate-pair1/` 的 preflight、JSON 构建日志、source-before/after、工具链与环境记录、固定副本 hash 和 Host 注入值；pair.json 为自述证据，不是认证。bootstrap 与真实矩阵查看各 `.local/lifecycle/<attempt>/`、日志中打印的保留 fixture 路径和 `inputs.txt`，核每个反例及预期错误。输出或 stage 出现 `NHC_TERMINAL_UNKNOWN`/creation-handle unknown、期限或观察失败时，监督器继续保留原对象并停后续启动；检查 direct-exit 不等于证明后代/Job/I/O 收束，须经理按同一原对象闭环。
