# N18-F01 诊断组合与最终 attempt2 窗口

2026-10-31，项目经理。原C接续C-R19；当前唯一员工执行者为C，B/D源码与暂存区冻结、no-build。D已结束定向诊断窗口，经理已核原件并完成串行整合。

## 基线与目的

原C树已由经理从 `7fce5b2e187a1147f4fcc784b5d07580eb6afe2c` 快进到 `7dc734b0832dcd888a4e67fda1d666deb990cd15`；NoManCode tree `9057a7999fc1c8a34322b6254a31fb946ec82bb3`，Rust tree `535566761693515f6087a1a306385487c580518d`。只有D授权的workspace_crash测试改变，生产代码和C原五文件不变，工作树clean。C须本人重新核收完整身份与冻结状态。

D定向6/0/0、Clippy/fmt0，只证明诊断候选本次通过；原C失败没有得到根因解释。新测试保存root/task/PID、真实kill/wait、原DB/WAL/SHM、独立queryable与recovery，使完整回归中若再次失败可归属。不得把诊断改动写成已修复产品，也不得因再次通过自动关闭N18-F01。

## 完整窗口

在原C树、原受控target缓存、独立 `next18-final-attempt2` 日志和TMP/TEMP下，重新显式 `cargo build --offline --locked --bin peachsh` 并记录实际CLI hash，然后依序完整执行：

1. `cargo check --offline --locked --workspace --all-targets`
2. `cargo test --offline --locked --workspace -- --nocapture`
3. `cargo fmt --all -- --check`
4. `cargo clippy --offline --locked --workspace --all-targets -- -D warnings`
5. `cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown`

执行、环境、首失败停止和源码冻结规则沿[原完整窗口](NEXT-18组件通过与最终五门禁窗口.md)；不改默认并发、timeout、断言或源码，不过滤用例，不执行ignored/live，不安装升级或发布。保留每步起止源码清单、三SHA/clean、UTC/退出、真实CLI前置及实际完整或部分suite统计。

保全13 runtime阶段、W08/A10实际可归属现场；新增两write根逐项记录target/root/task/PID/kill/wait/reaped、原始三件套、queryable/observed、独立recovery、真实文件与子进程日志。复制前后核字节/hash；原始DB/WAL/SHM不得打开查询或恢复。原attempt1失败及D诊断attempt1都保留。

若失败，立即停止后续命令，保全并报告具体原件，不就地返工或重跑。若全部通过，报告当前完整候选结果和原失败本次是否复现，同时明确N18-F01根因仍未知这一界限，交经理结合完整矩阵和实际现场作独立裁决。C无main整合权限；完整项目及正式Desktop等仍未完成。
