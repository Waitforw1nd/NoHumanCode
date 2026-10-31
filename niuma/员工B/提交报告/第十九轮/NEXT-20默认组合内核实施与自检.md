# NEXT-20 默认组合内核实施与自检

2026-11-01，员工 B。范围及验收依据为 [经理正式契约](../../../项目经理/任务/2026-11-01NEXT-20默认组合内核契约.md)。本报告只交付 B 的组合内核候选及组件自检，不代替 D 独立反例、C 公开 CLI 和最终组合门禁。

## 固定候选与所有权

- 独立树 `../nhc-b-default-composition`，分支 `codex/b/default-composition`；基线 HEAD `18e760c9e824e0014f56939758670139c6df1d55`。
- 候选 HEAD `de658989c11fa1777526eeea2c4f76d97dc9b2aa`；NoManCode tree `abd73485069f03e120e83640108d99cc75280d2e`；Rust tree `d223b20668ce8ef271bb8062b0fa4548d8dad5f4`。
- 产品提交依次 `edaa738d`、`46d5a0b3`、`de658989`；均仅涉及授权产品文件，人员文档未纳入该树提交。受测前后 status/index/unmerged 均空。
- `HostComposition` 是 lib 根模块，持同一 Store、workspace gate、active 与四运行域；Engine 兼容字段是相同 Arc。`HostComposition::new` 对 RecoveryService 的 Store 进行运行时指针一致性校验。生产服务不持有 Engine；测试探针仍引用 engine 的 cfg(test) 模块。

## 13 项实际入口

| 能力 | 组合及生产调用 |
| --- | --- |
| core | `HostComposition::builtin/new` 构造真实运行域和共享句柄；`Engine::assemble_composition` 由同一组合装配。构造测试验证 Store、gate、active 的 Arc 一致和错 Store 拒绝。 |
| session-log | `SessionService` 承担 Session、Turn、run/context、events 和相关 task 读取；HTTP 通过 `Engine::sessions` 使用同一 Store，不复制事件与序号。 |
| local-project | `ProjectService` 执行注册、配置、选择、查询和任务绑定验证；Engine、ToolCall、ReviewRecovery 使用该服务，写操作沿用同一 gate。 |
| workspace | `WorkspaceSafety` 持同一 gate、封装原 resolve/change_target/read_safe_file/atomic_replace；ToolCall 和审阅恢复实际调用它，现有文件安全实现与事务未重写。 |
| provider-xpeach | Engine 原 completion/route check 使用组合中的同一 `ProviderRuntime` Arc；其现有 prepare、generation、网络/预算 claim 和生命周期保持。 |
| file-read-search | ToolCall 通过同一 `ToolRuntime` definitions/acquire，项目与读准入经 ProjectService，实际工具绑定仍是 files 域。 |
| approval-policy | `ApprovalService` 提供实际 policy evaluate、审批 ensure/decide/claim；ToolCall 的审批及拒绝对账经此服务，存储事务仍为原权威。HTTP 读取经同一服务。 |
| file-write | 同一 files lease；ToolCall 使用 WorkspaceSafety 准备目标/读取，保留原 before/after/finish 事务与审批。 |
| shell | 同一独立 shell 域，ToolCall 经 policy、claim、lease 和原执行路径；网络告知与结果持久化未变。 |
| git-diff | Engine 方法委托 `ReviewRecovery::git_diff` 执行原算法，项目服务校验、同一 gate 和 active 冲突检查仍生效。 |
| checkpoint-restore | `ReviewRecovery` 实际执行 changes、latest_restore、create/read checkpoint、restore/restore_checkpoint 原算法；共享 gate/active/Store 和原测试探针。 |
| recovery | 正常 `main` 使用 `Engine::new_recovered`，由 RecoveryService 执行原一次 `Store::recover` 后装配同一服务；`--check` 保留提前返回，普通 `Engine::new` 不自动 recover。 |
| chat-agent | Engine 原任务期 acquire 与策略调用使用组合中同一 AgentRuntime Arc，独立 desired/lease/排空语义保持。 |

`GET /api/host/composition` 经既有 Host、Origin、跨站和 token 检查。13 项 capability 的运行域描述符从对应 runtime 实例现有 catalog 读取，状态直接使用原 plugins/runtime DTO；读取失败、缺失或接口不符返回错误。观察是逐组件快照，不代表已授权或全局原子快照。新测试还以注入的 Provider `1.0.7` descriptor 证明观察来自运行域实例。旧两个管理列表的实现及数量未改。

## 自检与现场

执行环境 Rust/Cargo 1.98.1、MSVC 14.43.34808、SDK 10.0.22621.0，offline/locked、jobs=2、dev/test debug=0、incremental=0。构建缓存复用原 B attempt4 的 target，新的 runner、日志和 TMP 在本树新 attempt 目录。各步骤原始 stdout/stderr、UTC、exit、工具链、前后 Git 和源码清单均保留。

| attempt | 固定候选 | 结果 |
| --- | --- | --- |
| [attempt1](../../../../../nhc-b-default-composition/NoManCode/rust-app/.local/next20-b-46d5a0b3-attempt1/) | `46d5a0b3` | check、CLI build 通过；lib 365 通过、1 失败，首失败后 fmt/Clippy 未执行。`process_test_fixture::process_fixture_failure_controls_reap_job` 的 `pipe-eof` 负控预期 stdout EOF，实际观察为 stderr EOF；不归因、不称修复，原始输出和 TMP 保留。check 另显示 engine 未使用导入。 |
| [attempt2](../../../../../nhc-b-default-composition/NoManCode/rust-app/.local/next20-b-de658989-attempt2/) | `de658989` | 五步全部 exit 0；lib **366 passed、0 failed、0 ignored**。check 无上轮未使用导入警告。 |

attempt2 五步依次为 `cargo check --offline --locked --workspace --all-targets`、`cargo build --offline --locked --bin peachsh`、`cargo test --offline --locked --lib -- --nocapture`、`cargo fmt --all -- --check`、`cargo clippy --offline --locked --workspace --all-targets -- -D warnings`。新增四项测试均通过：共享 Arc/disabled 观察、错 Store 拒绝、注入 Provider 描述符、HTTP token/Origin 和闭集形状。

attempt2 前后产品 178 个受 Git 跟踪文件清单与哈希一致；对首轮现场和既有 `next17-a10-evidence` 的 1,567 个保留文件比对，无缺失、变化或新增。postflight 状态 clean、无重型进程。attempt2 通过不能说明 attempt1 夹具异常已修复或确定根因。

## 移交

B 释放本轮唯一代码及重型构建窗口，候选交经理 review；D/C 尚未执行本候选的独立验证、公开 CLI 和最终联合门禁。第三方安装、WASM/process runtime、Desktop 和全项目完成均不在本轮交付范围。主仓人员档案及本报告由经理负责暂存；本独立树不做跨树整合。
