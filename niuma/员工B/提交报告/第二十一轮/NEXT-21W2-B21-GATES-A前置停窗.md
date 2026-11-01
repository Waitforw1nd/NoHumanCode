# B21-W2-GATES-A 前置停窗

2026-11-02，员工B。经理核收 FULL-C 受控矩阵并签同一固定源码的六步唯一门禁。源码 HEAD `f898f0b372bada37e2b962890c3dca23935c2f40`，产品 tree `77d0ecdf81b4ad90029bbc85db06271cc4478276`，Rust tree `43a9b604fb5e21b111cc831d98d2a92794d0962f`；前置及停窗 source 状态均相同、工作树/index 空，192份产品跟踪文件的前后 SHA256/长度比较0差异。

先将 FULL-C 218 份必要原件逐字节发布到 `B21-FULL-C原始证据/`，814626 字节，源/副 SHA256 复核0差异，`双hash清单.json` SHA256 `18eadb548258a5096c878d1159eb94cd49252a9ec3294602b90895d159c549e6`；保留原绝对路径/空白，不复制 EXE/target/tmp。FULL-C 三份固定二进制停窗后只读重核与原 manifest 一致。

新现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-gates-a-f898-attempt1/`；预先确认已安装 `wasm32-unknown-unknown`，按授权在本树 E 盘准备普通 `rust-app/target` 目录。本轮 E 缓存/TMP、offline/locked/jobs2/debug0/incremental0 的六命令写在 `start-gates.ps1`，workspace test 明确 `--test-threads=1`。外层隐藏 PowerShell 由本轮启动，PID 64544；启动动作返回后未持续持有其原句柄，不能称事后观察原对象 signal/退出码。`window-result.json` 是外围脚本自行记录的 exit_code=1。

UTC `2026-11-01T16:58:03.0062835Z` 开始，`16:58:04.1913276Z` 停止；在任何门禁子进程启动前失败。原因是本人新外围脚本使用 `Set-StrictMode -Version Latest`，执行项目 `build.ps1` 的现有工具链探测片段时，注册表对象缺失 `InstallationFolder` 属性触发脚本异常。`incident.json` 的 current=null，`toolchain.log` 空，未执行 `Add-Type` 的后续步骤或任何 cargo check/build/test/fmt/clippy/wasm check；全部六步均为未运行，不能将其记录为通过或 Cargo 失败。

脚本按首败保留原 `NoManCode/rust-app/.local/nhc-w2-window.lock`，SHA256 `496581d74aea962d0f25f92bca98cb6622cb3f83a3544bba96ef1b1f1db228f1`；未自行清锁、重跑、猜杀或删除任何现场。窗口后数字进程快照无匹配重型进程，仅为时点观察。来源证据为该目录 `incident.json`、`window-result.json`、`source-before.json`、`source-stopped.json`、两份 source 文件清单、`source-file-comparison.json` 与原 `start-gates.ps1`。

静态自查下一次外围脚本需在调用未经 StrictMode 设计的项目环境探测时避免继承 StrictMode，并把尾部只读常量变量 `$host` 改成任务专用变量（本机 `$Host` 选项 Constant/AllScope）。第二项是静态发现，未实际运行到那里，不能称为实测失败。以上均为外围 `.local` 验证脚本问题，不需改变 f898 产品源码；不得覆盖本次脚本/日志。下一步经理核前置终态与锁、裁定新现场和脚本修订；六门禁、fmt/Clippy/回归均未执行，W2 未最终验收或主线合入。
