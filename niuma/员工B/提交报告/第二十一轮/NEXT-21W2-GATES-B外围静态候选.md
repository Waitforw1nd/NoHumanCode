# B21 GATES-B 外围静态候选

## 经理静审后两点修订（当前有效）

`Hold-Unknown` 以 CreateNew 创建 `terminal-unknown.marker`，外层运行中仅检查此不可撤销标记是否存在，不读取持续覆盖的状态 JSON；原 supervisor signal 后再查标记和最终状态。标记创建失败时 supervisor 仍持句柄，外层最终按自己的总期限进入 unknown 持有；不能声称该故障一定即时落盘。外层创建 attempt/tmp 后，在 Add-Type 前设置进程级 TMP/TEMP。修订后 PowerShell parser 均零错误，未执行脚本或 Add-Type，产品与原锁不变。当前文件 SHA-256：

- `launch.ps1`：`642134661195f0c96afe92aaa75a8ddad6cac48a809b356458bb2c3b5952677f`（7009 字节）。
- `supervisor.ps1`：`f7baaa2872cf8e10e6f5d544562bf27866ffafa83561d5e7c6759ff933f36fb0`（10792 字节）。
- `GateProcess.cs`：`367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42`，未改。
- `manifest.json`：`9cc4dba82ac2a63d18e4ab44e79949e4a2f079cb10ec3b8f72dbafe9817cad31`。

下方表内两脚本与 manifest 哈希为此前版本，保留审查历史。

2026-11-02，员工 B。供经理审查；六门禁当前无动态授权，本次未执行 launcher、supervisor、Add-Type、cargo、测试或探针。产品未修改、未提交。独立树 HEAD `f898f0b372bada37e2b962890c3dca23935c2f40`，产品 tree `77d0ecdf81b4ad90029bbc85db06271cc4478276`，Rust tree `43a9b604fb5e21b111cc831d98d2a92794d0962f`；status clean、index 空，git diff --check 无输出。两份 PowerShell 脚本 Parser 均零错误；C# 仅静态阅读，未编译，不能据此声称运行通过。

完整外围候选在独立树 `NoManCode/rust-app/.local/b21-w2-gates-b-f898-prepared/`：

| 文件 | 字节 | SHA-256 |
| --- | ---: | --- |
| GateProcess.cs | 3909 | 367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42 |
| launch.ps1 | 7013 | 22c5854007fa27fe9d4b82aa3f1b1b53577e1dafdf76d64aaee2ec93ea5fbd84 |
| supervisor.ps1 | 10613 | 8e5c5f0d36dbe2d6ae245939d3a9fb11cf2ea45ca784ff5dda7b9547a57fe6f6 |

同目录 manifest.json 的 SHA-256 为 `faa706252e651417bc35108e6b166704683ddd96db6ad9aa5d95dbb31c20ccb8`。launch 要求完整 ExpectedHead、E 盘新 attempt 与 E 盘 target，并以 CreateNew 占锁；现有 GATES-A 锁存在时在创建现场前拒绝。监督者分别保有直接 CreateProcess 的原 supervisor/Cargo 句柄；记录 PID 和原句柄 creation FILETIME，Resume 后以原句柄 Wait/GetExitCode，不按 PID 重开或猜杀。超时、未知、wait/exit 查询失败及已有对象后异常走持有循环，先尽力记 incident，后续 signal 只记迟到事实，不自动解锁；signal 后循环 sleep，避免忙等。两层扫描终态 unknown 标记，signal 后再扫最终输出。每步 1200 秒，外层 7500 秒；到期意味着进入持有状态，不代表清理成功，也不保证监督者遭外部终止后仍持有句柄。日志写入失败会尽力记录但不能保证落盘。

直接创建 flags 为 `CREATE_SUSPENDED | CREATE_NO_WINDOW`，不调用 HasHiddenConsole；仅说明直接创建进程的创建标志，不能证明所有后代窗口行为。探测 build.ps1 的片段在同作用域临时关闭 StrictMode 后恢复，保留工具链环境；末尾变量为 `$hostArtifact`。六条命令顺序为 workspace check、debug peachsh build、workspace test（`--test-threads=1`）、fmt check、clippy `-D warnings`、wasm32 check；相关 Cargo 命令 offline/locked、目标目录参数化，fixture 和缓存行为仍须经理审阅。21 ignored exact、三个 helper 及付费 live 均不因该组门禁单独启动。

GATES-A 证据原样发布至 [B21-GATES-A原始证据](B21-GATES-A原始证据/)，12 件 82207 字节，源/副双 hash 清单 SHA-256 `2c4ce1f2ed12252dd3e4352ce3b792e2e1de4f58e1b3262287e3ba5075b767ca`，发布复核 0 差异，包括原锁副本；原始路径与内容不改。原锁仍在独立树 `NoManCode/rust-app/.local/nhc-w2-window.lock`，SHA-256 `496581d74aea962d0f25f92bca98cb6622cb3f83a3544bba96ef1b1f1db228f1`，未移动或删除。GATES-A 六步未启动；旧外围 PID 64544 由 Start-Process 启动而未保存原观察句柄，window-result 是脚本自报 exit 1，不能写成原句柄观测 signal 或退出码。旧 `$host` 冲突未运行到，仅是静态发现。

下一步由经理审查此固定外围候选及旧锁处置；不自行解锁、运行或重试。
