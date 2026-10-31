# NEXT-19 诊断组件 attempt1：完整 lib 首失败停止

2026-10-31，员工 C / 6.1 Terra。按照[静态通过与组件窗口](../../审查记录/第二十轮/2026-10-31NEXT-19诊断静态通过与组件窗口.md)执行固定候选 `a3279c8ce1ae3c9b81ee7eb5f9c699318f98c753` / NoManCode `d7255928f661d45039f2adebb846443434447588` / Rust `07688925b53c8858c1ec1a606c86ebe0355e8612`，分支 `codex/c/tool-call-workflow`。完整 lib 失败，组件未通过；没有重跑、修改源码或推断旧问题已解决。

| 顺序与命令 | UTC 开始 → 结束 | 结果 |
| --- | --- | --- |
| `cargo check --offline --locked --all-targets` | 12:49:12.0251588 → 12:50:05.1880978 | exit 0 |
| `cargo build --offline --locked --bin peachsh` | 12:50:05.9934771 → 12:50:55.6376177 | exit 0 |
| `cargo test --offline --locked --lib -- --nocapture` | 12:50:56.2924696 → 12:52:17.3025675 | exit 101，352 passed / 3 failed / 0 ignored |
| `cargo test --offline --locked --test approval_gate -- --nocapture` | 未执行 | 首失败停止 |
| `cargo clippy --offline --locked --all-targets -- -D warnings` | 未执行 | 首失败停止 |
| `cargo fmt --all -- --check` | 未执行 | 首失败停止 |

完整 lib 的三个失败为：

1. `server::worktree_http_crash_tests::w08_real_router_process_kill_wait_at_three_durable_boundaries`，本次 `created` phase 的 ready 等待超时；不是此次 claim phase 失败。
2. `command_execution::tests::s06_deadline_covers_inherited_pipe_after_direct_parent_exit`，接收原 descendant 证据时报 OS error 2，原日志记录找不到文件。
3. `git_worktree::tests::w06_inherited_pipe_deadline_keeps_partial_outcome_unknown`，读取 descendant-pid 时 OS error 2。两个继承管道失败的具体原因和后代是否曾启动均未证实，不能只凭缺文件归因环境或负载。

`worktree_management::tests::n13_dropped_caller_owned_worker_still_registers_once` 本次 ok；这不是旧 N19-F02 根因的证实或关闭。N19-F01/F02 和 N18-F01 仍保持各自历史证据及未归因边界。

## W08 本次逐 phase

原根相对总仓库 `../nhc-c-tool-call-workflow/NoManCode/rust-app/.local/next19-c-a3279c8-attempt1/tmp/`。`claim` 在 `.tmpklSW6U`：原 child PID 26300，已观察 listen、HTTP send、ready，kill 成功并 reap exit 1；restart PID 53132 已观察 listen，恢复验证 `unknown`、kill/reap exit 1。诊断日志记 guard/handler、Engine、前置/claim、worker 和 ClaimCommitted，原 HTTP transport error 出现在 kill 后。该 phase 观察到原目标停点及原断言，非整个 W08 通过。

`created` 在 `.tmpM0AfLh`：原 child PID 51196，listen 与 HTTP send 已观察；ready 原 10 秒超时，`original_http_finished=false`，随后 drop kill 成功、reap exit 1；无 restart，该测试未进入 `registered` phase。诊断 child 内单调 elapsed：guard enter 0.081460 秒、handler enter 0.083028、Engine enter 0.083805、prepare 0.091673→4.066689、项目 probe 4.066994→6.986129、claim 6.986498→6.999314、spawn 和 worker enter 约 7.000、ClaimCommitted 回调入口 7.000169。最后记录命令序号 52 在 9.695379 秒开始 spawn，子 PID 5076 在 9.699383 秒已 spawn、9.699496 开始等待；未见该命令结束或 created 停点记录。子进程启动、阶段累计和超时的记录使本次位置具体到 claim 后 worker，但没有证明该命令异常、其完成情况或旧 attempt 的相同原因。parent 的超时和 child 单调时钟起点不同，不直接相减。

claim/restart 日志共 252 事件，created 431 事件，均低于每 case 2048 上限；在相应 child log 和 lib stderr 未发现诊断写失败/上限消息。仍不能把缺事件当作绝对未执行。诊断文件同步写会影响墙钟调度，尤其 claim 返回到 worker spawn 前；后续结果必须保留此限制。

## N13 本次及保全

独立 fixture `.tmp4PloH5`，测试进程 PID 50336；诊断记录 prepare 0.008109→2.289425 秒、项目 probe 2.289679→3.858099、claim 成功 3.872029、worker/ClaimCommitted 约 3.8729，3.873133 观察 ready 且 caller pending，16.853175 记录 `n13_assertions_complete`。原 abort、worker 注册与回放断言随该测试本次 ok。N13 fixture 成功也保留；stderr 分别记录 `host.db`、`host.db-wal`、`host.db-shm` 均 copied 到 `raw-observed`。这是活库非一致观察副本，原 `host/` 在 Store 释放后只见 host.db；不以副本替代一致快照，也没有打开或恢复 DB。

## 其他原始崩溃现场

六组位于本次 `tmp/`，原 metadata 均记 kill=ok、wait exit 1、reaped=true，原 raw/query/recovery、信号、child log 与逐文件 hash 均保留：

| 原场景 | 原目录 | child PID |
| --- | --- | --- |
| T05 before_commit | `nhc-t05-sj9oCJ` | 51316 |
| T05 after_commit | `nhc-t05-dX5AVR` | 51360 |
| 旧只读 assistant 保存后 | `.tmp0Q2kHo` | 43256 |
| 脱敏拒绝 before_commit | `.tmpYoSzCB` | 43004 |
| 脱敏取消 before_commit | `.tmpzLS20w` | 51632 |
| builtin HostDenied after_commit | `.tmpAFLo3L` | 32428 |

## 环境、完整性与封存

工具 Rust/Cargo 1.98.1，MSVC 14.43.34808、SDK 10.0.22621.0；target、TMP/TEMP、日志及测试临时目录均在 C 独立树 E 盘，offline/locked、jobs2、dev/test debug0、incremental0。开跑前该树旧 fixture 依赖的 `rust-app/target` 不存在，本人建立空目录并在 runner 前置核存在；未清理、安装或升级。CLI `target/debug/peachsh.exe` 为 29,813,760 字节，SHA256 `358090964d12a97fbab3919504b395d83d229d14de816fd690af3caa52cbebb7`。

开跑前及每步后、最终均保存 Rust 工作区受 Git 管理的 140 项输入字节清单，五份清单 SHA256 一致 `53c731c0d0e33e860081049724743331393ff501cf7197e6ea8d23ec01d98f3b`；前置逐项与 Git blob 比较 140/140 一致。工作区和父路径未见 `.cargo` 目录；外部已安装工具链、离线依赖缓存不在此 140 项内。每步 Git 记录及最终三 SHA、branch/status/index/unmerged 均为固定候选和 clean。

运行前本人树保留现场 0 文件，结束外部 `next17-a10-evidence` 新增 120 文件、读取错误 0；本次 tmp 另完整登记 1426 文件、读取错误 0，不把旧现场清单当作新现场清单。保全时不打开或恢复原 DB。结束时 runner 的重型进程列表空；额外只读核查所记录的 5076/51196/53132/26300/50336 当时均未列为现存进程，且未发现关联 Git/Cargo/rustc/peachsh 进程；S06/W06 未获得后代标识，不能证明其历史生命周期。

[发布清单](NEXT-19诊断组件attempt1证据/manifest.json) SHA256 `cd3e54753c48005b055385cb677383ee5b9dcd9c23499383c01321a872366fcc`，48 个日志、清单、runner 和各关键 metadata/诊断文本副本逐项登记原路径/长度/hash、发布路径/长度/hash、路径替换及次数；原/发布 hash 已逐项重核无不符。原件保留在 C 树，测试原数据库和完整现场以原件及 tmp 清单为准；副本中的文本路径转换只为可移植阅读。B/D 保持冻结，已释放运行窗口；下一步等待经理独立审证和按实际失败分别裁决，不自行重跑或开始 T13/T14。
