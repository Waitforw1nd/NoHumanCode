# NEXT-19 完整独立组件 attempt2：approval_http 三项失败

2026-10-31，员工D / 6.1 Terra。固定候选的all-targets check、显式CLI build、完整lib均通过；十四integration命令中`approval_gate`通过，第二套`approval_http` 13通过、3失败，命令exit101。按窗口首失败停止，其余十二套、Clippy和fmt未执行，完整独立组件及NEXT-19联合验收未通过。没有修改源码或重跑。

本人核D独立树`../nhc-d-tool-result-tests`、分支`codex/d/tool-result-tests`，HEAD `cbdba68205ba40c6c9e23777520c054eb8653ffd` / NoManCode tree `a7f36b202e8f71da94df72ca2a9be436c22db191` / Rust tree `55aea660485681924a5148c1a962ab87aa5c9965`。开跑前及结束三SHA相同、status/index/unmerged空，规定重型进程空；B/C冻结。

| 步骤 | 命令与UTC起止 | 结果 |
| --- | --- | --- |
| 1 | `cargo check --offline --locked --all-targets`，14:55:07.399–14:55:17.453 | exit 0 |
| 2 | `cargo build --offline --locked --bin peachsh`，14:55:23.181–14:55:29.040 | exit 0 |
| 3 | `cargo test --offline --locked --lib -- --nocapture`，14:55:34.702–14:56:34.105 | exit 0，362通过/0失败/0忽略；P01、P02均实际运行ok |
| 4 | 十四integration完整命令及UTC见原`04-integrations.json`，14:56:39.796–14:57:14.780 | exit 101；`approval_gate` 35/0/0，`approval_http` 13/3/0；新增P03 `n19_redacted_denial_and_cancellation_reconcile_after_runtime_restart`实际运行ok |
| 5 | `cargo clippy --offline --locked --all-targets -- -D warnings` | 未执行 |
| 6 | `cargo fmt --all -- --check` | 未执行 |

第4步完整命令为`cargo test --offline --locked --test approval_gate --test approval_http --test tool_session_turns --test tool_result_crash --test workspace_changes --test workspace_crash --test checkpoint --test checkpoint_http --test network_authorization --test network_http --test plugin_management --test plugin_http --test runtime_management_workflow --test tool_runtime -- --nocapture`。仅前两套有实际执行输出；`tool_session_turns`、`tool_result_crash`、`workspace_changes`、`workspace_crash`、`checkpoint`、`checkpoint_http`、`network_authorization`、`network_http`、`plugin_management`、`plugin_http`、`runtime_management_workflow`、`tool_runtime`均未执行，不能计为通过。

## 实际失败及静态线索

`approval_http`三项失败：`ah01_ah09_ah10_safe_dto_order_and_task_isolation`、`ah02_ah08_ah12_decision_snapshot_conflict_and_concurrency`、`ah11_real_busy_decision_is_retryable_and_corruption_is_not`。原stderr均记录在`tests/approval_http.rs:255`的`card`内`ensure_approval(...).unwrap()`收到`tool result conflicts with persisted state`。失败测试的临时Harness数据库没有可归属留存，不能凭日志声称已取得失败时持久历史或已动态证明具体调用序号。

只读源码对照显示，三个测试均有同一Task再次调用`card`以追加新的assistant调用组；该helper在每次追加后保存并创建审批。`src/store.rs:2974`的`safe_approval_call_digest`对之前消息组调用`validate_prior_groups`，而`src/store.rs:3083`要求先前调用组有对应tool结果；再次追加尚未闭合的组可产生该Conflict。此是与现场错误一致的静态定位线索，不把缺失的失败DB或执行调用序号冒充动态证据，也不由此判定生产行为应如何修改。P03新四场景实际通过不代替原`ah12`中因该失败未到达的后续断言；用户要求收尾，本轮不派发修复。

## 真实现场与证据边界

完整lib六组真实进程场景均保留raw/query/recovery独立目录和原metadata；metadata记kill=ok/reaped=true，raw中DB/WAL/SHM长度与SHA-256均逐项核对一致。路径均相对本轮`tmp/`：

| 场景 | 原目录 | PID / phase | 对应副本目录 |
| --- | --- | --- | --- |
| T05 before | `nhc-t05-PeStwJ` | 48632 / before_commit | `raw`、`query`、`recovery` |
| T05 after | `nhc-t05-UadeSr` | 57148 / after_commit | `raw`、`query`、`recovery` |
| 旧只读 | `.tmp40KYj1` | 56796 / agent_after_assistant_save | `legacy-raw`、`legacy-query`、`legacy-recovery` |
| 取消 | `.tmp1YPK15` | 4836 / before_commit | `proof-raw`、`proof-query`、`proof-recovery` |
| 拒绝 | `.tmpWWVSnY` | 17956 / before_commit | `proof-raw`、`proof-query`、`proof-recovery` |
| HostDenied | `.tmpm9io0T` | 57192 / after_commit | `host-denied-raw`、`host-denied-query`、`host-denied-recovery` |

各task/call、字节hash及进程信号详见原`process.json`或相应`*-process.json`；本人只核字节，不打开或恢复原数据库。N13 `.tmpxxwSdk/raw-observed`为活库观察副本，不称一致快照。完整lib中的W08三个目录`.tmprbmn9H`、`.tmpHF78pU`、`.tmpkWOjFl`均记原child live/HTTP pending、kill及reap、非timeout/connect输送错误、restart reap；日志的观察顺序不扩大为OS级因果证明。S06/W06、四夹具负控随完整lib通过，相关现场列在发布清单。

## 环境、完整性与封存

Rust/Cargo 1.98.1、MSVC 14.43.34808 Hostx86/x64、Windows SDK 10.0.22621.0；offline/locked、jobs2、dev/test debug0、incremental0、默认测试并发。复用D树`.local/next19-d-3a1ea4f-attempt1/target`，本轮日志/TMP独立；本轮显式CLI `debug/peachsh.exe`长29,816,320字节，SHA-256 `d7092f21853d2de945bc967d75c62454163f6005fc1e0ce7e5688de01e3eb24b`。

开跑前、每步后及结束六份141项输入清单逐字节等于固定Git blob、读取错误0，清单SHA-256均`31208c929b347786693671340667f7c8dd1663bc5d0fddc0f8a0712f46842a3f`。旧现场含前几轮3379文件，结束4959文件，新增本轮TMP 1580文件；旧文件缺失/变化0、读取错误0。原件根`../nhc-d-tool-result-tests/NoManCode/rust-app/.local/next19-d-cbdba68-independent-attempt2/`。

[发布清单](NEXT-19完整独立证据cbdba68-attempt2/manifest.json) SHA-256 `0366416d6f9b71d436bfa9d28763366861f4d453702414065e7ae96a9c2a4d95`，列120份原/发布runner、日志、输入/现场清单、夹具文本、W08生命周期/诊断及六场景metadata的长度、SHA-256和路径转换；已逐项验证。DB/WAL/SHM等其余现场以原件和`retained-after.json`为准，未复制为声称一致的活库快照。attempt1及补充06原件保持。

窗口已释放，D树源码/index冻结；按用户收尾要求，不自行返工、重跑或启动C新实施。N18-F01和N19-F01/F02/F03/F04仍OPEN_UNATTRIBUTED；十二integration、Clippy、fmt、C公开T13/T14、NEXT-19联合验收及后续Desktop仍未完成。
