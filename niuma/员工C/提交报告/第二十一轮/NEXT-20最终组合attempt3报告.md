# NEXT-20 C 最终组合门禁 attempt3 报告

2026-11-01，员工 C / 6.1 Terra。受测 `5d7e26857b8192912412a1456f3aa0b091dd6288` / NoManCode `5ebccc0f2cc4232d67060563dcee0d5e6f6d309b` / Rust `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`，六步全部 exit 0；本次有效结果不追认历史失败根因。原 attempt1 runner 中断无 test 退出，attempt2 Clippy 101、wasm 未执行，原件与[失败记录](NEXT-20最终attempt1中断与接续.md)保留。

| 命令 | UTC 2026-10-31 开始 → 结束 | exit |
| --- | --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 21:25:41 → 21:25:42 | 0 |
| `cargo build --offline --locked --bin peachsh` | 21:25:43 → 21:25:48 | 0 |
| `cargo test --offline --locked --workspace -- --nocapture` | 21:25:49 → 21:28:59 | 0 |
| `cargo fmt --all -- --check` | 21:28:59 → 21:29:01 | 0 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 21:29:01 → 21:29:09 | 0 |
| `cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown` | 21:29:10 → 21:29:11 | 0 |

北京时间为 2026-11-01 05:25–05:29，精确时分秒见各命令 JSON。workspace 62 个原结果块合计 **837 passed / 0 failed / 1 ignored**，包含 helper 入口；唯一 ignored 是付费真实 Provider live，未执行。lib 378/0/0；`composition_cli`、`first_user_workflow`、`tool_call_workflow` 各 1/0/0 实际执行。各 suite 的顺序配对与计数见[逐 suite 清单](NEXT-20最终attempt3证据/logs/workspace-suite-counts.json)。保留 `proc-macro-error2 v2.0.1` future-incompat 提示。

## C01–C12 证据与限制

C01/C02：B 的 [typed组合根和真实服务接线审查](../../../员工B/审查记录/第十九轮/2026-11-01默认组合内核候选审查.md)说明单 Store/gate/活动状态、真实调用及反向依赖边界；其后 D 的[完整独立审查](../../../员工D/审查记录/第十八轮/2026-11-01NEXT-20完整独立验证通过.md)覆盖受测组合，C 最终全量在此基线上继续。

C03/C04：上述 B 审查与 D 独立缺失/错误绑定、构造与生命周期测试提供受限范围证据；不可注入或进程外第三方 runtime 不在本轮。C 的真实 CLI 假服务反例覆盖坏结构、依赖、版本及不可信读取，不代替内核故障注入。

C05/C07：D 覆盖四域、lease/排空及审阅恢复；本轮原 `first_user_workflow` 在同一真实 Host 禁用四域后实际 Diff/Checkpoint/冲突恢复、同库重启再核 disabled/revision/Provider计数/副作用，并按重启后查询 revision 恢复、继续原故事。该 suite 的三次 P07 Host kill/wait PID 35012、21956、33396；原有旧故事未删除。

C06/C08：D 的审批/Project/事件独立测试与本轮 `tool_call_workflow` 的真实公开审批、拒绝无副作用、批准一次、kill/restart 不重发和新 Turn 承担对应范围；本轮并无第三方 Agent 权限绕过证明。T13 原根 `.tmpz3WbbT` 的 PID 54668、53100 记录 kill/wait exit 1，停机 raw/query 分离；三文件摘要见原 metadata 和 tmp 清单，不对原 DB 做额外查询或恢复。

C09/C10：`host composition` 真实 CLI 提供 JSON/human 只读入口、13 项闭集及四域状态；`composition_cli` 对重复键、缺项/多项/错误依赖版本、嵌套 DTO、合法大小边界/超限、可观察重定向目标、断线/持续连接超时及 403/404/500 安全分类执行反例。原 `first_user_workflow` 对照旧两套列表并在关闭/重启后核持久状态；`tool_call_workflow` 继续原公开开发链。本次结果不等于四域原子快照或用户已授权全部能力。

C11/C12：B/D 审查覆盖 schema17 和旧回执/事件兼容，C 四路径未改 schema、Store 或依赖；本报告六门禁及 147 受测输入完成最终组合验证。联合验收、主线整合及项目整体完成不由本报告代行。

## 现场、输入及发布

原根 `../nhc-c-default-composition/NoManCode/rust-app/.local/next20-c-5d7e2685-final-attempt3/`。W08 claim/created/registered 原根 `.tmpkF8NPA`、`.tmpzYTsAm`、`.tmpeld5v6`，原 child PID 46216、48708、55472；其恢复与真实 kill/wait 按 lifecycle/containment 原件保留。六个旧崩溃场景 T05 before/after、旧只读、拒绝、取消、HostDenied 原根分别 `nhc-t05-7mEcC7`、`nhc-t05-hKIsea`、`.tmpt1s5cD`、`.tmps5vzfc`、`.tmppPFO4N`、`.tmpOnVZpy`，各 process metadata 留存。经理[最终源码与现场核验](../../../项目经理/审查记录/2026-11-01NEXT-20最终源码与现场核验.json)独立核原件及七进程场景/T13 raw/query；未为封存运行额外恢复。

本人核 147 个 Rust 工作区受 Git 管理输入开跑前与 Git blob 字节一致；前置、每步后及结束共八份清单一致，原清单 SHA256 `78606f7cd055c8da7dd03d765f173681eda5287c812166363cee145b08938507`。旧现场 8091 文件不变、结束 8211（新增 120），本轮 tmp 2213 文件，读取错误均0。E盘独立 tmp/log，复用原 C 树已核 target，offline/locked/jobs2/dev/test debug0/incremental0，旧 `rust-app/target` 前置存在；Rust/Cargo 1.98.1、MSVC 14.43.34808、SDK 10.0.22621.0。

显式 build 后 CLI 29,979,136 字节、SHA256 `92bac905b4541f95500b2a5fee6cc0bee081ea012f759491ee54beac3b247d8e`；test 后磁盘 CLI 30,964,736 字节、SHA256 `287225c205f29697beaaec70167fa52659cabe6f72259f5087a68870472a2425`。两次是不同时间点磁盘文件，不臆称各进程加载了哪一哈希。

[发布清单](NEXT-20最终attempt3证据/manifest.json) SHA256 `800c743df639c3f2731d45ea886681f9ab85220f152b47830fcd418cfd6a87de`，65 份原/发布文本逐项记录路径、长度、hash 和路径转换并核无不符；完整现场以原件和 tmp 清单为准。runner final heavy=[]；受测结束时 C 树 clean，未清理旧现场。

经理授权结果登记后的 docs-only 候选 `ee33ef1a9acf51a20841eb587034cbe4a6a1297e` / NoManCode `897cc8510c0c56118df04c2c1dbcea9c999e539f` / Rust `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`，相对受测仅 CLI 文档一段，Rust tree 完全一致、status/index/unmerged 空；未重跑产品。本人冻结释放，等待经理联合审证和整合；N18-F01、N19-F01/F02/F03/F04及N20-F01等历史未归因边界保留，默认组合切片不等于完整项目或第三方 runtime/Desktop 完成。

