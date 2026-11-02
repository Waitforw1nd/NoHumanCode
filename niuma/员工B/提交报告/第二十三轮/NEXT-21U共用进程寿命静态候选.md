# B23 NEXT-21U 共用进程寿命静态候选

2026-11-02，员工B Jason，同角色接续。经理已核本轮实际 gpt-6.1-terra/high，接续原B23，C仍唯一调用者写入。仅供经理静审，不是组件或U验收。

## 固定对象

- 基线 `3dc44827ec9b501bd85913e43c28627eb2aaa0ae`；独立分支 `codex/b/process-lifetime`；最终HEAD `27c7f60176c394a51339680092166c0dbce25b4d`，产品tree `012665bb46ee4afffb446dc74e3d7761320434cd`，Rust tree `afa0765e16c10b70f56175597a09908fef8db6b9`。正常连续提交 `0aa70cd4`、`518448a3`、`27c7f601`；未改写历史。提交后树clean、index空。
- 相对员工树 `NoManCode/rust-app/`：`src/process_lifetime.rs` SHA256 `359b1d6eb1f035bfc13c800c3e61b3cc5d26c6be2dc48d0d2f16330fd483c94e`；`src/process_lifetime_tests.rs` `2c3a9edfd11dcd92530607b96846f95510a7acf5727a16b8e5461f8bac899e76`；`examples/process_lifetime_probe.rs` `08e1e9172079219207a9a3206b19d9bfb8b2ffb1283ede1bedf51cd0afc1bf88`；`tests/support/process_lifetime/run-fixed.ps1` `a1ec4b5fcd354a87922a9d1f94f1a7ad9eef97e05194eb7b0c1eb7b0dd96e1ba`。复用未改 `tests/support/wasm_worker_process/DirectProcess.cs` 为 `7c85ab87fe34ba201015ed078dc7aada3c48cba02bffcf3418af1a007e33949d`。另仅Cargo既有windows-sys增加Globalization feature，lib注册模块/测试。

## 实现及接口

共享Supervisor new/resize/submit与不透明DomainLease仍按[在途契约](NEXT-21U进程寿命API与持有契约.md)。resize与占槽同锁；unknown先同步seal再通知，settle同锁裁决/转移后锁外消费，持槽覆盖holder和watchdog结束；caller Drop只请求停止。创建时Job-list与挂起membership验证后执行，原对象、reader、Job及票据随holder持有。

本轮修正原对象 `WAIT_OBJECT_0` 与 `GetExitCodeProcess` 失败的混淆：后者封门并保留sticky Unknown，前者事实不撤销；原process已signal、Job空且reader真实结束时走cleanup-only，不能制造退出码或成功Capture。终止故障测试注入仅cfg(test)，失败时仍维持原对象与槽，故障解除后可验证迟到收束。

## 源码覆盖映射（均未运行）

| 契约 | 测试/夹具草稿 |
| --- | --- |
| 非零、未启动，正常树、脱离stdio多后代、截断并drain | `normal_nonzero_and_prestart_rejection`、`detached_multi_child_waits_for_real_tree_zero` |
| 超时、取消、调用者Drop、输出溢出回收 | `timeout_and_cancel_reap_original_tree_without_sealing`、`dropped_caller_requests_cleanup_without_immediate_unknown`、`output_limit_termination_keeps_tree_until_real_cleanup` |
| Job观察故障、reader Err已join对仍阻塞、membership和创建后交付停点 | `failed_job_observation_stays_unknown_until_late_real_cleanup`、`reader_error_joins_and_cleans_but_unknown_is_sticky`、`live_reader_keeps_slot_even_after_process_and_job_finish`、`unknown_on_membership_and_created_hold_retains_then_cleans` |
| signal与退出码故障、真实未signal负控、sticky/迟到一次释放 | `exit_code_failure_returns_unknown_then_releases_once`、`exit_code_failure_preserves_signal_and_waits_for_io`、`unknown_is_sticky_and_ticket_consumed_once_after_cleanup` |
| 终止故障、活后代、拒新及恢复 | `termination_failure_retains_live_tree_then_recovers_without_reopening` |
| 缩容保留、竞争、阻塞结算与watchdog占槽 | `resize_preserves_existing_slots_and_uses_new_limit`、`resize_races_submit_without_erasing_existing_slots`、`blocking_settle_does_not_hold_decision_lock_or_release_slot`、`watchdog_real_exit_is_covered_by_slot` |
| 创建归属及禁止breakaway | `breakaway_independent_controls_and_component_denial`：独立允许/拒绝Job控制与组件拒绝；`parent`以原Child对象复制句柄，核harness创建时间 |
| 参数、cwd、显式环境及父sentinel排除；NUL/重复环境名前置拒绝 | `argv_cwd_environment_roundtrip_and_invalid_prestart` 和probe `roundtrip` 二进制UTF-16回执 |

创建停点位于CreateProcess已返回后的挂起对象交付，并非真实内核CreateProcess卡住的动态证据；缺失目标程序的创建失败与前置输入拒绝各有草稿，无法证明所有内核故障。capacity竞争可覆盖实际交错，但单次运行不保证两种获锁顺序。测试Domain只模拟封门，真实C域票、同域准入及双Host集成仍须C接线和独立验证。

## 外围及验证边界

`run-fixed.ps1` 接收预先构建的测试程序、probe和预期SHA，检查三Git对象、clean/index、实际tracked Rust工作区文件路径/长度/SHA，记录前后快照并在每例前复核；当前tracked列表静态数为162，含runner及复用的DirectProcess.cs。先在E盘attempt内创建并设置TEMP/TMP，再Add-Type；固定副本、每例独立目录、显式exact串行、原process句柄等待与退出码检查。未知、超时或失败时保留runner、原句柄与锁并停窗。它不执行构建；build/no-run、外层监督、全局串行协调、源/副证据双hash发布仍需经理另行审定准备，不能将单脚本描述为正式门禁通过。

静态检查：指定三个Rust文件 `rustfmt --edition 2024 --check` 通过，PowerShell parser零错误，基线到HEAD `git diff --check` 通过。没有运行Cargo、编译、测试、runner、真实产品或probe；测试源码和Win32行为均未验证，不能视为运行通过。下一步经理集中审候选及外围；动态窗口须单独签发，C接线和U整体验收仍开放。
