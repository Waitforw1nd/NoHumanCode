# 员工C（Workspace真实崩溃补证 第1.0轮报告）

2026-09-26。任务 `C-R5-CRASH-SUPPORT`；实际模型 `gpt-6-sol`；经理正式派发。接收当刻未采集时钟，首次可核对登记时间 `2026-09-26T03:43:30+08:00`，执行标识 `C-R5-CRASH-SUPPORT-20260926-034330`。本轮未宣称验收。

## 基线与交付

- 原开发基线 `dd57adfc9321454651bdcdabb3a081bc95091600`（B 未验收中间版），经理快进 B 修复后组合基底 `083eae51820a008d02197c58b0dcfdf2ff7ba775`。C 固定测试提交 `0bdaaac237b15829e971e5cc98b6f3ec97dd8d20`，只新增 `NoManCode/rust-app/tests/workspace_crash.rs`，SHA-256 `4AB4A41F717D4E2A36A8ED1EA829F7975A0BB995C3C6DDC1B68FBD19F86B2D68`。独立工作树 `../nhc-c-workspace-crash/`、分支 `codex/c/workspace-crash-tests`；提交后工作树干净。共享档案单独写在本仓库 `niuma/员工C/`，未操作共享 Git 暂存区。
- 测试仅用本机 mock Provider、临时数据库和工作区；无付费 live、release build 或产品后门。临时 SQLite 触发器的长递归查询仅装入测试 DB，分别阻塞写 finish 与恢复逐路径 outcome。父进程确认文件副作用、持久 prepared/claimed 与 SQLite 写事务占用后，`kill` 并 `wait` 确认专属子进程退出，再按生产启动顺序 `Store::recover`、`Engine::new` 重开。每段等待有 20 秒上限；`ChildGuard` 兜底回收子进程。测试临时目录由 `TempDir` 清理。

## 实测事实

| 窗口 | 前置屏障与停止 | 重开结果 |
| --- | --- | --- |
| W07：write_file 文件已写、finish 事务未提交 | 文件为 after；change 仍 prepared；`BEGIN IMMEDIATE` 为 busy；kill/wait | change unknown、不可恢复；文件仍 after；重复恢复拒绝、无自动重写 |
| W07：finish 已提交 | change finished 且子进程仍在下一次 mock Provider 响应处；kill/wait | change 保持 finished，文件仍 after、可恢复；不重复写 |
| W08：真实 HTTP POST restore 文件已还原、outcome 未提交 | 文件为 before；restore 为 claimed；DB 写事务 busy；kill/wait | restore unknown、逐路径未伪计 complete、稳定 restore_id；GET changes 返回 unknown receipt；重复 POST 为 409 同 ID，不再次写；崩溃时 HTTP 调用随子进程终止，未收到响应，不伪称当次收到 409 |
| W08：HTTP restore complete 已提交 | 子进程已收到 HTTP 200，父进程见 complete 行后 kill/wait | complete receipt 与 restore_id 稳定；文件被外部再编辑后重复 restore 返回原 receipt，不覆盖新内容 |

测试程序的两个 `crash_child_*` 入口在普通定向运行中无环境变量时立即返回；四条父进程场景实际执行。定向测试输出计数为 6/6，通过数含两个入口空运行；实际场景 4/4。

## 验证与限制

- 在 `083eae5 + 0bdaaac` 上，以独立 `CARGO_TARGET_DIR=../../.local/target-crash`、现有 `build.ps1 -Action check` 引入显式 VS/SDK 环境：`check` exit 0；`cargo test --locked --test workspace_crash` 6 passed/0 failed；`cargo fmt --all -- --check` exit 0；`git diff --check` 无输出。
- 初次未设本机 MSVC 环境时 `build.ps1` exit 1，提示缺少工具链；补齐 `VSINSTALLDIR`、`VCToolsInstallDir`、`WindowsSdkDir` 后通过。初次恢复夹具遗漏生产 `Store::recover`，当时读到 claimed 导致 1 个测试失败；修正夹具后复跑通过，未把失败改写为通过。原始终端日志未单独归档，以上为本轮命令输出事实；不存在可声明的日志前后 hash。
- `cargo clippy --test workspace_crash --locked -- -D warnings` exit 1，阻断在 B 负责的 `engine.rs:889,1177,1350` 与 `store.rs:817,952` 五条 lint；已向 B 和经理报告，C 未修改 B 文件。B 新提交后须复跑。标准全量 test/wasm-check 未由 C 执行，留给 B/经理组合门禁。
- 经理追加要求后，C 在固定 `0bdaaac` 之上尚未提交的同一测试文件加入 W08 `changes[0].restore_state == "unknown"` / `restorable == false` 断言。在 B `083eae5` 上定向复测 exit 1：实际 `restore_state="pending"`，预期 `"unknown"`，定位为 B 的 recover 投影缺口，已交 B。原 6/6 是追加断言之前的真实结果，不能用它宣称新组合通过；待 B 修复后形成测试第二提交和复测证据。
- 补强版还核 W07 approval execution_state、持久 tool 消息、tool_result 和 finished 安全事件的事务一致性，并核 W08 逐路径 outcome、complete 后真实 HTTP 重复 POST。一次并行定向运行出现 W07 finish 后 20 秒超时；审查发现父进程可能在 child-task-id 创建后写入前读取空串。夹具已改临时文件写完后同目录 rename 发布，并要求非空且 tasks 表有对应行；子进程 stderr 留在自有临时目录供失败诊断。修复后 W07 两场景连续三轮各 2/2 通过。此次反例不记为生产故障。
- W07 prepare/claim 后 FS 前、W08 restore claim 后 FS 前缺可证明停点。只在公开 API 上轮询状态后 kill 有不可接受竞态；经理已交 B 采用仅单元测试编译的私有 phase barrier 补证。C 未把这两段标为通过。现有测试不证明跨进程 Engine 并发互斥，也不宣称任意外部文件系统竞态安全。

## 接续

收到 B 修复 Clippy 与前副作用窗口的固定 SHA 后，经理协调更新测试分支，再复跑定向、fmt、Clippy；经理/A 审查并决定最终组合验收。C 保持测试文件写权，未经派发不修改 B 源码或 `workspace_changes.rs`。
