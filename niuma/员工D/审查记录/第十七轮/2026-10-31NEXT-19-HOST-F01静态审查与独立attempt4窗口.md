# HOST-F01静态审查与完整独立attempt4窗口

2026-10-31，项目经理。APPROVED_FOR_INDEPENDENT_RUN。全文审[静态报告](../../提交报告/第十七轮/2026-10-31NEXT-19-HOST-F01合法Host初始化静态修正.md)与单文件5+/1-，实际字节与Git blob一致、clean及diff check通过，见[固定核验](../../../项目经理/审查记录/2026-10-31NEXT-19-HOST-F01固定候选核验.json)。变更仅导入fresh_settings和历史写入前初始化，原全部断言、公开Host启动及两次kill/wait不变；生产保护不变。尚未动态闭合HOST-F01。

唯一执行者原D；原树`../nhc-d-tool-result-tests`、分支`codex/d/tool-result-tests`，受测HEAD `2f1bc071ca4494404c8d0370604240aa29a93b55`，NoManCode `15f9e726327197f5105293d9997ad800a2cd4f9a`，Rust `e64f9c159d311b5134cd94230e9a8f71932c6205`。D静态阶段已completed释放，现恢复唯一动态窗口；B/C冻结。

本人先重核三SHA/branch/status/index/unmerged、实际输入与blob、旧现场清单及无重型进程，然后沿原环境六步串行：

1. `cargo check --offline --locked --all-targets`
2. `cargo build --offline --locked --bin peachsh`
3. `cargo test --offline --locked --lib -- --nocapture`
4. `cargo test --offline --locked --test approval_gate --test approval_http --test tool_session_turns --test tool_result_crash --test workspace_changes --test workspace_crash --test checkpoint --test checkpoint_http --test network_authorization --test network_http --test plugin_management --test plugin_http --test runtime_management_workflow --test tool_runtime -- --nocapture`
5. `cargo clippy --offline --locked --all-targets -- -D warnings`
6. `cargo fmt --all -- --check`

首失败停，不修改/重跑/过滤用例、不调整超时或默认并发。Rust cwd、Rust/Cargo1.98.1、MSVC14.43.34808 Hostx86/x64、SDK10.0.22621.0只读；原E盘target复用，独立attempt4日志/TMP，offline/locked/jobs2/debug0/incremental0；rust-app/target旧夹具前置保持。显式记录CLI实际产物hash。六步骤各自命令/UTC/exit和全部suite真实计数；未执行不得记通过。

保留attempt3及之前全部原件，前轮6594条清单外新增封存文件也纳入before inventory。六lib真实kill raw/query/recovery继续保留，本次tool_result_crash若成功须另列首次与第二Host实际启动、kill/wait/恢复、原历史结果不变/无tool_start及raw hash；不得借用lib场景替代。准确区分原件、查询副本和发布路径转换；只打开查询副本，发布清单逐项长度/hash。

结束报告实际结果、封存第十七轮报告/清单、更新身份、postflight和重型进程检查后释放冻结。失败不自主修复。完整通过后仍须经理审证、原C T13/T14及联合整合，不能直接合main或宣称全项目完成；历史未归因问题状态保持。
