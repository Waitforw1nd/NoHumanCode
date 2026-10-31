# NEXT-16 C-R17 首候选与组件计划

2026-10-31，员工 C / 6.1 Terra，执行 ID C-R17-01-20261031-E01。依据正式 v1.0，独立树 `../nhc-c-first-user-workflow`、分支 `codex/c/first-user-workflow`，从 `f2b5b6b071167372e9da14112890a3e48d11731d` 开始；初始双 tree 为 NoManCode `84f630de7154d12f2051796cd4d18fc50aa44258`、Rust `98d3f344d274cdff219c0d0047c4dae26cb23c5e`，status/index/unmerged 均空。NEXT-15 联合验收与 main 整合见经理记录，旧树保持冻结。

首候选 HEAD `e810979b3eaffe701542e778ae7eddb3dc85b962`、NoManCode `7084f92095db7c1cb86fd799751bd7351aa6221f`、Rust `c636c0a81e9512a9700965abc4d472d9fcc43573`；status/index/unmerged 空。仅改 `NoManCode/rust-app/tests/first_user_workflow.rs` 和 `NoManCode/docs/cli-first-release-acceptance.md`，独立树本人提交，主仓 index 和跨树整合归经理。未改生产、schema、迁移、权限、凭据或公共协议。

P07 单一测试设计从缺失数据库启动真实 Host，以公开 CLI stdin 输入合成凭据，创建与选择 Project、预算与授权；同一 Run/Session 读列搜、拒绝与批准写、审批 Shell 测试、查询事件、真实 Git diff、checkpoint、外部冲突、恢复与旧 Task 封存、新 Turn，再真实 kill+wait、同库重新 bootstrap 并继续。只准备文件/Git/mock，未以 Store/SQL 预注入业务数据；检查 shell 副作用不因文件恢复或重启撤销/重复，检查 Provider 次数、预算及 receipt。现阶段均为测试代码与计划，不是实际通过或已验收。

实际静态检查：`rustfmt --edition 2024 --check NoManCode/rust-app/tests/first_user_workflow.rs` exit 0；`git diff --check` 与提交前 `git diff --cached --check` exit 0。未编译或运行测试。已请求经理安排独立串行窗口：在本人树 `NoManCode/rust-app` 使用现有 MSVC/SDK、E 树独立 TMP/TEMP/target/log、offline/locked/debug0/incremental0/jobs2，执行 `cargo test --locked --test first_user_workflow -- --nocapture`，保留原始首结果及准确候选和退出码。窗口前保持 source/index clean/no-build；原 owner 修编译问题不降低断言，真正产品问题回经理指派原 owner。最终五门禁仍需含 B/D 新组合后精确放行。

下一步第一条：等待经理准许串行组件窗口；获准后先本人核候选和独立环境再运行，报告真实输出与失败，不预写通过。完整阶段一和全项目均未完成。
