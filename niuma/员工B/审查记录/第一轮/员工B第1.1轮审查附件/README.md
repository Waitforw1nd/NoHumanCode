> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../../路径与可移植性.md)。

# 员工 B 第 1.1 轮复审附件

日期：2026-09-23。对应 [员工 B review 1.1](<../员工B review 1.1.md>)。

## 实际结果

原有 workspace 测试：94 通过、0 失败、1 个付费 live 忽略，退出 0；fmt/clippy/wasm-check 退出 0。

审查探针：6 通过、3 失败、12 过滤，退出 101。6 个通过项来自上轮反例；旧候选冲突测试本次额外确认错误类型为 `PredecessorChanged`。

| 新增探针 | 实际结果 | 意义 |
| --- | --- | --- |
| `review_b11_incomplete_function_must_not_dispatch` | 失败；新增一个 Turn、调用 mock Provider 一次 | B-R3 仍放行已配对但 function 缺名/参数的畸形历史 |
| `review_b11_partial_fixture_must_have_its_own_delta` | 失败；Run 有一个 delta，新 Task 没有，drop 后旧 Engine 仍 busy | B-R5 的重启测试前置条件不成立；不等于已经证明生产 recover 丢草稿 |
| `review_b11_session_and_turn_decode_failures_are_not_missing` | 失败；两个存在但解码失败的对象均返回 NotFound | B-C1 诊断建议只修了部分实体，不另立 P1 |

## 文件

- `review_b11_probes.rs`：负责人探针；include 同目录的 `session_turns.rs`。
- `session_turns.rs`：被审查的 B 正式测试快照，仅复现用，不覆盖共享文件。
- `reviewed-src/`：本轮 domain/engine/store/repository 源码快照，方便后续对照行号和实现。
- `review-test.log`：原有 94 项测试的实际输出。
- `review-gates.json`、`review-clippy.log`、`review-wasm-check.log`：门禁结果；fmt 无标准输出。
- `review-probes.log`、`review-probe-result.json`：9 项审查探针结果。
- `review-environment.log`：真实 PS7 版本与 pwsh 路径。
- `review-source-manifest.json`、`source-recheck.json`：快照与最终源码哈希；B 文件一致，C 文件已继续变化。

这次没有把任何探针写入共享正式测试，也没有修改生产实现。未访问真实 Provider/用户运行实例，未执行付费 live。所有测试库和凭据来自本机临时 mock 夹具。

## 复现方式

固定源码副本：`./.local/temp/fufu-b11-review-30b643772f`。不能只 checkout HEAD 代替包含未提交 A/B 修复的基线。

把本目录 `review_b11_probes.rs` 和 `session_turns.rs` 放入隔离副本的 tests 目录。使用真实 PowerShell 7，在隔离副本根目录运行：

```powershell
# 使用独立 target；不要与员工正在运行的 Windows EXE 共用输出位置。
$env:CARGO_TARGET_DIR = Join-Path (Get-Location).Path 'target'
. .\build.ps1 -Action check
cargo test --locked --test review_b11_probes review_ -- --nocapture
```

被审查版本预期 6 通过、3 失败。原 12 项员工测试仅被 include 编译，在此命令中由 `review_` 过滤。本轮固定源码快照的原 94 项通过与这组反例结果分别记录。

负责人实际复用了自己此前创建的隔离编译缓存 `./.local/temp/fufu-c-review-22cad04b86\target`；构建输出明确显示编译源位于 B 1.1 副本，没有拿 C 的旧源码作为审查对象。

恢复原有 test/fmt/clippy/wasm-check 门禁前将审查探针移出 tests；不要把故意失败的反例和未按正式格式整理的脚本算成员工原有门禁失败。后续正式修复由 B 吸收正确断言与夹具，C 文件保持其自己的所有权。
