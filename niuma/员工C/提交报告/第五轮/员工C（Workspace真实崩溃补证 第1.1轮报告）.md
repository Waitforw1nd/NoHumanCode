# 员工C（Workspace真实崩溃补证 第1.1轮报告）

2026-09-26，`C-R5-CRASH-SUPPORT`，实际模型 `gpt-6-sol`。接收时钟未采集，首次登记 `2026-09-26T03:43:30+08:00`；执行标识 `C-R5-CRASH-SUPPORT-20260926-034330`。本报告承接[第1.0轮报告](<员工C（Workspace真实崩溃补证 第1.0轮报告）.md>)，保留其早期失败事实，不宣称经理验收。

## 固定输入与范围

- B 修复候选 `948e0cccf4f852b265c38a08c6b516470a0476e5`，经理把它与 C 首个测试提交组合为 `557a4c51e408440cd29e1d8f4f02afffa1686d58`。C 仅在 `NoManCode/rust-app/tests/workspace_crash.rs` 补强，第二固定提交 `1f2a98101c457008b8e8725716d2d90979b0d0a1`；最终文件 SHA-256 `8D6F41209C2518C1983C8D958EE1E85BA4697AF75A5DA6E03D198D66E57C6E85`。独立分支 `codex/c/workspace-crash-tests`、工作树 `../nhc-c-workspace-crash/` 干净；B 实现、`workspace_changes.rs`、共享 Git 暂存区未由 C 修改。
- 子进程通过真实 Engine/Store 与本地 mock Provider 执行批准写入，通过真实 HTTP POST 执行恢复。每个被终止的子进程都由父进程 `kill` 后 `wait`，重开先 `Store::recover` 再创建 Engine。SQLite 长查询触发器仅装入临时 DB。写 finish/恢复 outcome 窗口都以实际文件副作用、持久 prepared/claimed 和 DB 写事务占用三项前置事实定位，等待有界。完成后窗口先核 finished/complete 持久事实再 kill。

## 结果

| 窗口 | 重开核对 |
| --- | --- |
| W07 写后 finish 提交前 | 文件 after，change 与审批 execution_state 均 unknown；持久 tool 消息 0、tool_result 事件 0、finished file_backup 事件 0；不可恢复、不自动重写 |
| W07 finish 提交后 | 文件 after，change 与审批均 finished；持久 tool 消息、tool_result、finished file_backup 各 1；恢复资格保持，无重复副作用 |
| W08 HTTP 恢复文件后 outcome 提交前 | 文件 before；parent receipt、逐路径 outcome、change.restore_state 均 unknown，restorable=false，restored=0；重启 GET changes 的 restore_id 稳定；重复 POST 为 409 同 ID，不再次写。崩溃时同子进程的 HTTP 调用被终止，未收到当次响应，不伪称已收到 409 |
| W08 HTTP 恢复 complete 提交后 | 子进程实际收到 HTTP 200；重开 receipt、逐路径 outcome 均 complete，restored=1、ID 稳定；外部再编辑后重复真实 HTTP POST 返回 200 和相同 receipt，文件保留外部内容 |

`workspace_crash` 计 6/6，通过数包含两个无环境变量时立即返回的 child 入口；实际父进程崩溃场景为 4/4。临时子进程 stdout/stderr 写本测试自有临时目录，父进程仅接受原子 rename 发布且非空、已存在于 tasks 表的 task ID；这修正第1.0轮的一次并行超时夹具竞态。修正后 W07 两场景连续三轮各 2/2 通过。临时目录和子进程有清理兜底，不触及其他 Host。

## 门禁与未覆盖

- 固定组合定向测试 exit 0（6 passed/0 failed）；`build.ps1 -Action check` exit 0；`cargo fmt --all -- --check` exit 0；`cargo clippy --test workspace_crash --locked -- -D warnings` exit 0；`git diff --check` 无输出。所有命令使用独立 target，未运行 release build、付费 live 或 C 侧全量 test/wasm-check。原始与相对化日志、起止、退出码及前后 SHA-256 见[证据清单](C-R5证据/清单.md)。
- B `083eae5` 旧组合上新增断言真实失败：recover 后 `restore_state="pending"`，预期 `unknown`，定向 exit 101；原始失败日志与后续成功日志均保留。B `948e0cc` 修复后同测试通过。第1.0轮旧 Clippy 五项失败也已在新组合的本次定向 Clippy 关闭；旧事实不改写。
- W07 prepare/claim 后副作用前与 W08 restore claim 后副作用前由 B 的私有确定性屏障负责补证，C 本文件没有证明这两段；直接轮询后 kill 存在竞态，不可冒充证据。B/经理仍负责组合五标准门禁、全部 W 项与最终审查；本报告不是完整 NEXT-03 验收。
