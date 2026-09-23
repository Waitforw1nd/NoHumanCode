> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 C：HTTP 与 SSE 传输契约 第 1.2 轮报告

日期：2026-09-23。对应 `员工C review 1.1`。本报告不宣布审查通过。第 1 轮和第 1.1 轮报告保留。

## 1. 本轮范围

只处理 C-R5 的 H7 测试握手。C-R1～C-R4 以及 H8、H9、H10、health 按复审结论保持，没有重写 `server.rs`。

修改文件：

- `rust-app/tests/http_contract.rs`
- `员工任务/第一轮/员工C HTTP-SSE契约.md`：只补说明一次解码后仍含字面 `%` 的参数名会被拒绝，例如 `other%25=1`。这比“所有未知参数都不拒绝”更严格，但不影响当前 `after` 客户端。
- 本报告

没有修改 B 的 Engine、Store、Repository、domain 或 `session_turns.rs`，没有改 schema/protocol，没有新增 Turn 写路由，没有全仓自动格式化，没有提交或发布。

## 2. C-R5 H7

原问题：Provider 进入后 `started.notify_waiters()` 只通知当时已经在等待的一方。测试若在 POST 返回后才创建 `entered.notified()`，这次握手会丢失并在 3 秒后超时。

修法：进入握手和释放握手都改成一对一的 `notify_one()`。它会保存一个 permit，后创建的等待仍能消费。正式回归在等待 `entered` 之前，先有界 `yield_now` 到 `calls == 1`，固定“Provider 已通知、测试随后等待”的顺序，然后仍执行原断言：

- 任务在消费 SSE 帧并断开后仍 `busy`
- 释放 mock 后任务 `completed`
- Provider 调用总数为 1
- 重连收到严格更大的 seq

没有删除 `entered` 等待、忙状态或完成断言，也没有用长 sleep 掩盖顺序。

## 3. 实际验证

真实 PowerShell：`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`，版本 7.6.5。

| 命令 | 结果 | 退出码 |
| --- | --- | --- |
| `cargo test --locked --test http_contract h7_disconnect -- --test-threads=1` | 1 通过，0 失败 | 0 |
| `pwsh -NoProfile -File build.ps1 -Action test` | 库 43、adversarial 4、final_acceptance 9、http_contract 11、runtime 10 通过；live 1 忽略。`session_turns` 16 项中 15 通过，`t9_cancel_restart_and_resume_boundaries` 失败 | 101 |
| `pwsh -NoProfile -File build.ps1 -Action fmt` | 失败。差异在 B 的 `repository.rs` 和 `session_turns.rs`，不是本轮文件 | 1 |
| `pwsh -NoProfile -File build.ps1 -Action clippy` | 通过；仅有既有 `proc-macro-error2` 提示 | 0 |
| `pwsh -NoProfile -File build.ps1 -Action wasm-check` | 通过；同样只有既有提示 | 0 |
| `git diff --check -- rust-app/tests/http_contract.rs` | 通过 | 0 |

定向 HTTP 测试使用项目脚本同等 MSVC 环境。没有启用付费 live 测试。

## 4. 未完成与边界

- 全量测试的新失败是 B 的 `tests/session_turns.rs`：`t9_cancel_restart_and_resume_boundaries` panic 为 `Cannot start a runtime from within a runtime`。这不是 C 的生产或 H7 失败。本轮按所有权没有修改该文件。
- 全量 fmt 被 B 文件的现有格式差异挡住。只检查了本轮拥有的 `http_contract.rs`。没有全仓自动格式化。
- 第 1.1 轮复审记录的 94 通过、1 忽略不再代表当前工作区。当前快照的新证据是上面的 T9 失败。
- 新 Turn HTTP 写入口仍未挂载。门禁通过不等于整体验收通过。
