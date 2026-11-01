# B21-GATES-B attempt2 执行记录

2026-11-02，员工B，进行中。经理在 [中断核收与attempt2签窗](../../审查记录/第二十一轮/2026-11-02GATES-B中断核收与attempt2签窗.md) 核验旧证据并签唯一窗口。启动前 f898/双tree、clean/index、192源码和四份prepared哈希均一致；B路径/cache及原两PID进程快照无匹配。UTC 2026-11-01 23:05:11 原锁独占打开成功，源与目的路径检查通过且目的不存在，管理性保全迁移：

- 原位置：`../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/nhc-w2-window.lock`
- 保全位置：`../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-gates-b-f898-attempt1/nhc-w2-window.original.lock`
- 迁移前后 SHA-256 均为 `a810bed31bfb6ad756df4f901cadcd976f7db078ee6bb59ca31c5c4d3e7c103a`；源已不存在。旧清单与原件不改，迁锁不证明原句柄终态或所有后代已回收。

前份事故报告“用户交接称 session”一语来源澄清：session 32621 的 Unknown process id 系经理/原B只读核验结果，并非用户本人自述的直接观察；原始现场不改。

下一步启动已审 runner 的全新 attempt2，持续持有工具会话并观察；首败或未知停止，不重复 FULL-C 21矩阵。

## attempt2 前置停窗及证据

UTC 2026-11-01 23:05:26 启动，工具会话退出1；launcher 记录原 supervisor 句柄 PID 77504、creation FILETIME 134380479268394301、UTC 23:05:28 signal、exit 1。supervisor 的 incident `current=null`，在环境探测报 `Missing Visual C++ x64 build tools` 后停止；六门禁均未启动，不是产品编译或测试失败。before/stopped 的192文件 hash 和长度逐项一致，HEAD/双tree及 clean/index 一致。新锁保留，SHA256 `6262379b285922f249312805f1d134d2110bfadd8452b52945a25d99a8c41491`；当前B路径匹配进程快照为空。未重跑/迁移该锁。

[attempt2 原始证据](B21-GATES-B-attempt2原始证据/)包括顶层11件与新锁副本，共12件、72353字节，复制前后源和副本 SHA256/长度一致、复核零差异；[双hash清单](B21-GATES-B-attempt2原始证据/双hash清单.json) SHA256 `6dfa07af5e891901446de3357f2b09c52b50f714adcce2cf689eb0a6c5404727`。原文件不改；tmp、EXE、target未复制。

## 工具链发现的静态差异

旧 GATES-A `start-gates.ps1` 在调用 build.ps1 之前显式设置 `VCToolsInstallDir` 为 `%ProgramFiles%/Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808`、`WindowsSdkDir` 为 `%ProgramFiles(x86)%/Windows Kits/10/`。已审 GATES-B supervisor 未显式设置这两个入口，而是继承父进程环境，再调用 build.ps1 自动发现；此次调用环境两变量均为空。`vswhere.exe` 存在，但按 build.ps1 的组件过滤查询未返回 installationPath；本次失败在 MSVC 检测，尚未执行 SDK 检测。GATES-B attempt1 原 environment 记录解析到同一 MSVC 和 SDK，但没有逐项记录其父环境，不能断言当时发现途径。

只读检查现有 MSVC 目录下 `bin/Hostx86/x64/cl.exe`、`link.exe`、`lib.exe`、`lib/x64`、`include` 均存在；`bin/Hostx64/x64` 对应三个文件本次不存在，build.ps1 支持 Hostx86 回退。SDK 的 `Lib/10.0.22621.0/{ucrt,um}/x64` 及 `Include/10.0.22621.0/{ucrt,shared,um}` 均存在。探测失败不等于安装缺失；这些是文件存在检查，未在本轮运行编译器。

未来仅经经理另行裁定锁与签窗后，候选调用环境可在启动现有 launcher 的同一 PowerShell 会话中显式设定如下两项，令子进程继承；不需安装、注册表修改或改产品。此为静态方案，尚未执行验证：

```powershell
$env:VCToolsInstallDir = Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808'
$env:WindowsSdkDir = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'
```

下一步经理核验前置事故及新锁，决定后续验证窗口；本任务未完成。

## attempt3 静态调用候选（待经理签窗）

补充只读逐项检查：MSVC 目录中 `lib/x64/libcmt.lib`、`include/vcruntime.h` 存在；SDK 10.0.22621.0 的 `Lib/ucrt/x64/ucrt.lib`、`Lib/um/x64/kernel32.lib` 及对应 Include 下 `ucrt/stdio.h`、`shared/sdkddkver.h`、`um/Windows.h` 均存在。前述 SDK 路径中的 Lib 和 Include 后均包含版本号。它们包括 build.ps1 显式检查的 kernel32/ucrt 文件和三个 include 目录；其余文件存在性不能替代实际编译。Hostx64 分支本次未发现三个工具文件不证明原安装发生变化。

候选命令在 B 树 `NoManCode/rust-app` 当前 PowerShell 内设置两项入口后启动原已审 launcher；attempt3 和独立 tmp 由 launcher 在签窗后创建，缓存复用原B target，旧 attempt2 与锁仍保留。调用者需持续持有工具会话，明确不存在新attempt目录并重新核对经理签窗条件。以下仅静态候选，未执行：

```powershell
$env:VCToolsInstallDir = Join-Path $env:ProgramFiles 'Microsoft Visual Studio/2022/Community/VC/Tools/MSVC/14.43.34808'
$env:WindowsSdkDir = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits/10'
$attempt = Join-Path (Get-Location) '.local/b21-w2-gates-b-f898-attempt3'
$cache = (Resolve-Path '../../../nhc-b-tool-call-application/NoManCode/rust-app/.local/next19-b-a7fe2626-attempt4/target').Path
& (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File '.local/b21-w2-gates-b-f898-prepared/launch.ps1' -AttemptDir $attempt -TargetDir $cache -ExpectedHead 'f898f0b372bada37e2b962890c3dca23935c2f40'
```

构建脚本先尝试 Hostx64，当前三个工具不存在，再尝试已存在的 Hostx86/x64。SDK 选择按其实际探测顺序执行，预期目录并非实测的下一窗口结果。无产品/runner修改、安装或注册表修改。
