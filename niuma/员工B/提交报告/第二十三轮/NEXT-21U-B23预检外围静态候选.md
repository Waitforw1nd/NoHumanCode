# B23 PRECHECK 外围静态候选

2026-11-02，员工B Jason。经理核收组件静态候选后，仅准备编译/lint外围；未启动本包、未运行Cargo、未编译C#、未运行测试或probe。固定HEAD `27c7f60176c394a51339680092166c0dbce25b4d`，产品tree `012665bb46ee4afffb446dc74e3d7761320434cd`，Rust tree `afa0765e16c10b70f56175597a09908fef8db6b9`，树clean/index空。

实际包为员工树 `../nhc-b-process-lifetime/NoManCode/rust-app/.local/b23-precheck-27c7f601-prepared/`。来自旧B21受控原件的新副本，不改旧树现场；所有SHA256按字节核：

| 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `launch.ps1` | 9954 | `63868d13bb9b84e31457e5b453b6f345ecc844da2f0d97a339ddb6b783f4f84a` |
| `supervisor.ps1` | 12486 | `e4ec1635f4fcb39b2e54ef019c71a167d51660b959add9da52be3501f561aaf3` |
| `manifest.json` | 723 | `9f9d6fdcd521844592eadf5f210bc76b80534377694a32fe0197652f01203e4d` |

两份PowerShell经Parser静态解析零错误；C#未编译。`launch.ps1` 要求完整ExpectedHead及上述manifest SHA，核三脚本源/复制字节及末次hash。未来启动参数另给全新attempt及其`target`子目录；attempt必须是本Rust工作目录`.local`直接子目录，禁止覆盖现有目录。

## 拟执行次序及边界

1. 先获取Git common-dir下唯一 `nhc-heavy-window.lock` 的CreateNew闩锁，核候选三对象、clean/index、所有tracked产品路径/长度/SHA并记录；当前静态列表196项。每步前后再采样、逐项比较初始源码，失败终止。此锁跨本仓Git工作树可见，但只能约束使用同一锁的参与者；旧W2树自己的历史锁及C的旧runner不会自动参与，经理仍须统一串行调度并处理各自旧闩锁，不能把它宣称为系统范围强制排他。
2. E盘attempt中建立TMP/TEMP及私有CARGO_HOME，从现有用户Cargo registry作本地复制；新target限定在attempt内。现有MSVC 14.43.34808 Hostx86/x64及SDK 10.0.22621.0只读，显式设置工具/库/头路径；jobs2、dev/test debug0、incremental0、offline，清除继承的Rust flags/wrapper。工具路径、字节、hash与环境、target工件各步快照会记录。不会下载；Cargo的编译过程可能正常执行构建脚本与过程宏，但不运行测试或probe。
3. `cargo check --workspace --all-targets --offline --locked --target-dir <attempt/target>`；原进程signal且exit0并完成快照后，才执行 `cargo clippy --workspace --all-targets --offline --locked --target-dir <attempt/target> -- -D warnings`。每步1200秒，外层监督2700秒；首个非零立即停，不预写通过。私有组件若触发dead_code，保留真实失败，按经理授权后作最小过渡措施并新提交、新attempt，不扩大API/关闭整crate lint。

外层隐藏且挂起启动supervisor，内层同法启动每个Cargo；均登记原handle的PID、creation FILETIME、命令、开始/结束、signal/exit和stdout/stderr。原句柄等待失败、超时、创建时间或退出码不可读、异常或unknown标记均不猜PID/强杀：保留原句柄、写incident并持续持有锁，迟到signal仅记证据，不自行清除unknown。确定的非零或脚本失败也保留锁文件供经理核查；只有完成及最终核对成功才释放。外围进程崩溃会留下锁文件，但无法保证崩溃进程仍持有句柄；需人工核实，不得自行开启新窗口。Cargo原进程signal本身不是对子进程树的独立归零证明，该限制须在签窗审查中考虑。

后续build/lib-no-run/probe、20矩阵和余门禁均不在此包中，须按预检结果另行准备与签窗。准备包不是执行证据；复核源码/工件采样、缓存容量和跨树gate协调后由经理裁定是否签窗。
