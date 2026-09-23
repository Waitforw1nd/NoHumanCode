> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../../路径与可移植性.md)。

# 员工 B 第 1 轮审查附件

审查日期：2026-09-23。对应 [员工 B review](<../员工B review.md>)。

这些文件用于查阅与复现，不是已经合入正式测试的交付。负责人没有修改共享 Rust 源码或 C 的文件。

## 文件

| 文件 | 含义 |
| --- | --- |
| `review_b_probes.rs` | 6 个审查探针，通过 include 复用同目录的 B 夹具 |
| `session_turns.rs` | 审查时 B 的原始测试快照，仅用于复现，不替代正式测试文件 |
| `review-probes.log` | 修正探针夹具后的实际执行输出：6 个期望断言失败，退出 101 |
| `review-baseline-test.log` | 原有 workspace 测试实际输出：83 通过、1 个 live 忽略，退出 0 |
| `review-clippy.log`、`review-wasm-check.log` | 原有源码门禁实际输出，退出码均为 0 |
| `review-gates.json` | fmt/clippy/wasm-check 的执行退出码；fmt 没有标准输出 |
| `review-environment.log` | 本次实际 PowerShell 版本及 pwsh 解析路径 |
| `review-source-manifest.json` | 隔离快照关键源文件 SHA256 |
| `source-recheck.json` | 审查结尾与共享源文件哈希对比；B 文件一致，C 的 server 已继续变化 |

## 结果解释

- `review_old_event_must_not_reopen_historical_turn`：B-R1。实际错误地把 T1 判成 latest 并接受历史 resume。
- `review_stale_predecessor_snapshot_must_be_rejected_or_refreshed`：B-R2。真实 resume 完成后，独立 Store 仍接受漏掉中间消息的候选。
- `review_malformed_message_must_fail_before_commit_and_provider`：B-R3。畸形上下文被提交并调用 mock Provider。
- `review_multiple_agents_must_be_refused`：B-R4。多 Agent Session 被接受。
- `review_wrong_workspace_must_not_be_silently_replaced`：B-R4。错误项目路径被静默替换后接受。
- `review_database_failure_must_not_be_classified_as_not_found`：B-C1 非阻断诊断建议。请求已拒绝，但已有行的解码错误被归为 NotFound；当前枚举允许 missing/corrupt 合并，不能把这个探针当作违反现有枚举定义的硬性验收。

五条业务反例加一条诊断建议，不是“原有六项测试失败”。最初 DROP agents 的探针夹具被外键提前拒绝，已更正为行解码故障；本附件探针及日志来自更正后重跑。

## 复现方式

请在隔离源码副本执行，不将本附件批量复制回共享生产目录。原审查副本位于：

`./.local/temp/fufu-b-review-50496aff95`

副本来自未提交工作区，不能只 checkout HEAD 代替。关键源码版本见 manifest；后续 B 修复后，旧 API 可能需要相应调整探针。

1. 将本目录 `review_b_probes.rs` 放到隔离副本 `tests/review_b_probes.rs`；同一 tests 目录使用本附件版本的 `session_turns.rs`。
2. 使用真实 PowerShell 7，在该隔离副本根目录执行以下命令。脚本先配置项目既有 MSVC 环境；不执行 live。

```powershell
# 先确认当前位置是隔离副本，不是 ./rust-app。
$PSVersionTable.PSVersion
(Get-Command pwsh).Source
. .\build.ps1 -Action check
cargo test --locked --test review_b_probes review_ -- --nocapture
```

在被审查版本上预期退出 101，具体实际结果见日志。修复后前五项业务断言应通过；第六项是否改变取决于采用的错误契约，不得据此伪造“六个既有需求全部闭合”。

3. 原有门禁应在不含 review_b_probes 的副本执行，避免把本次故意保留的失败断言或未格式化的审查脚本算作员工原有测试/格式失败：

```powershell
pwsh -NoProfile -File build.ps1 -Action test
pwsh -NoProfile -File build.ps1 -Action fmt
pwsh -NoProfile -File build.ps1 -Action clippy
pwsh -NoProfile -File build.ps1 -Action wasm-check
```

负责人执行时仅设置 `CARGO_TARGET_DIR=./rust-app\target` 复用编译缓存；源代码、临时数据库和 Provider 均来自隔离测试环境。全部测试凭据为 B 夹具的合成值；未执行付费模型测试。
