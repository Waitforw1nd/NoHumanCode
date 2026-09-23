> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../../路径与可移植性.md)。

# 员工 C 第 1 轮审查附件

日期：2026-09-23。对应 [员工 C review](<../员工C review.md>)。

## 文件与证据范围

- `server.rs`：被审查的 C 源码快照。
- `review_c_probes.rs`：C 原 11 项测试和夹具的完整副本，末尾追加 7 项负责人审查探针；没有改共享正式测试。
- `review-probes.log` / `review-probe-result.json`：最终有效夹具重跑，2 通过、5 失败，退出 101。
- `review-http-contract.log` / `review-http-result.json`：原独立 HTTP 测试 11 通过，退出 0。
- `review-gates.json`、`review-clippy.log`、`review-wasm-check.log`：fmt/clippy/wasm-check 均退出 0；fmt 无标准输出。JSON 中 test=101 是首次全量构建失败，不能解释成 HTTP 测试失败。
- `review-test.log`：首次共用 target 时的 LNK1104 输出。随后使用独立 target，没有删除或终止别人的缓存/进程。
- `review-test-isolated.log`：独立 target 的全量检查过程；停在 B 的 T3，不是全量通过日志。
- `review-stopped-test.json`：负责人仅停止自己隔离副本内阻塞的 session_turns 测试进程的路径证据。
- `b-intermediate-session_turns.rs`：当时 B 中间态夹具快照，用于解释同步 channel 阻塞；不是 C 应修改的文件，也不说明 B 最新版本状态。
- `review-source-manifest.json` / `source-recheck.json`：快照关键文件哈希及最终对照；C 文件一致，B 测试已变化。
- `review-environment.log`：真实 PowerShell 7.6.5 及可执行路径。

## 7 项探针结果

| 探针 | 结果 | 对应结论 |
| --- | --- | --- |
| `review_encoded_duplicate_cursor_is_rejected` | 失败，实际 200 | C-R1：编码名未计入重复参数 |
| `review_encoded_invalid_cursor_must_not_replay` | 失败，实际 200 并重放 seq=1 | C-R1：非法编码游标被当作缺省 0 |
| `review_corrupt_run_identity_is_internal` | 失败，实际 400/request_failed | C-R2：关系身份损坏错误分类 |
| `review_invalid_path_is_structured_json` | 失败，实际 text/plain 400 | C-R3：Path 默认拒绝绕过错误体 |
| `review_bad_kind_after_success_returns_error_frame` | 失败，Axum panic，客户端 UnexpectedEof | C-R4：SSE 构帧字段未验证 |
| `review_health_schema_failure_is_safe` | 通过 | health 故障返回安全 500 |
| `review_read_failure_after_success_keeps_cursor` | 通过 | 同一流先成功后 JSON 读取失败，after 正确且一次 error 后 EOF |

这不是员工原有 11 项测试失败。初版探针曾使用未声明的事件名 `review.probe`，在正常 Store 插入校验处提前失败；已改用合法 `delta`，再对测试库注入损坏，并完整重跑。本附件保存的是更正后的探针和输出，不把无效夹具当作实现漏洞。

所有故障注入只操作夹具临时数据库。没有用户实例、生产数据或付费 Provider 调用。

## 复现

原隔离副本：`./.local/temp/fufu-c-review-22cad04b86`。源码来自未提交工作区；只检出 HEAD 无法复现同一基线。后续修改后的版本需按实际 API 调整反例。

将本目录 `review_c_probes.rs` 复制到隔离副本的 `tests/review_c_probes.rs`，使用真实 PowerShell 7，在副本根目录执行：

```powershell
# 确认当前目录是隔离副本，使用独立编译目录避免与并行员工抢占 EXE。
$env:CARGO_TARGET_DIR = Join-Path (Get-Location).Path 'target'
. .\build.ps1 -Action check
cargo test --locked --test review_c_probes review_ -- --nocapture
```

被审查版本预期 2 通过、5 失败，退出 101。该文件前面的 11 个员工原测试被 `review_` 过滤，仍用于编译夹具。

原 HTTP 测试独立检查：

```powershell
cargo test --locked --test http_contract -- --test-threads=1
```

运行原有 fmt/Clippy 门禁前，将审查探针移出 tests；探针是反例附件，未按正式交付格式整理，不应被算成员工原有格式问题。不要在共享工作区删除、忽略或改写 B 的测试来追求全量通过。
