# NEXT-18 完整组合独立组件窗口

2026-10-31，项目经理。原 D-R16 继续，唯一执行人 D；B/C source/index/no-build 冻结。先核三方完整组合，再开放 D 单一重型窗口，不派发新任务或新员工。

## 固定组合与接收

B 组件 a26363f1 已通过；经理串行 C273895d、D36b73d 到 B 后，由 B 仅在 lib.rs 登记两个真实 D 测试模块，固定 `67841d0610374179f056e27e4a0060fc619081ac`。NoManCode `251c949b09ef3a7adb07c32f0834e1e154737d30`，Rust `872776408d4f141d400399bd6605605cc049ed66`。

经理已核四行登记差异及 Windows/test 门控与现有 DPAPI 夹具一致，双方持续冻结、clean 后快进到 D 原树，见 [整合记录](../../../项目经理/审查记录/2026-10-31NEXT-18-B到D完整接线整合.json)。D 开跑前须本人核收分支、完整三 SHA、status/index/unmerged 空及无其他重型进程。经理观察不代替本人接收。

## 实际窗口

按顺序，首失败停止；新组合源码/index 全程冻结：

1. `cargo check --offline --locked --all-targets`
2. `cargo build --offline --locked --bin peachsh`，核同 target 的 debug/peachsh.exe 存在。
3. `cargo test --offline --locked --lib -- --nocapture`
4. `cargo test --offline --locked --test runtime_management_schema -- --nocapture`
5. `cargo test --offline --locked --test approval_gate --test checkpoint --test project_migration --test workspace_changes --test host_configuration --test host_configuration_schema --test project_context -- --nocapture`
6. `cargo clippy --offline --locked --all-targets -- -D warnings`
7. `cargo fmt --all -- --check`

原 E 树 target/TMP/TEMP/log、已有工具链只读、offline/locked、debug0/incremental0/jobs2、准备旧 fixture 的 rust-app/target 不变。不降低并发、不增加 timeout、不跳断言。日志/metadata/命令/UTC/退出/源码清单独立记录，失败后未执行的步骤如实标记。

崩溃现场须保留 raw DB/WAL/SHM、阶段/PID/signal/kill+wait、独立 queryable 快照与两次新进程重开证据；发布副本逐项记录原/发布哈希，不覆盖旧失败。若有编译或测试失败，先保全报经理定向交原 owner，不擅改 B/C 产品。D 自有修订亦先报真实失败，重新固定后再放行。

本窗口优先验证完整组合的独立反例及历史 schema 夹具；C 公开 CLI 组件与最终五门禁随后串行执行，不能以本窗口替代。主线仍 NEXT-17，完整项目未完成。
