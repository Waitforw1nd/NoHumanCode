> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../../路径与可移植性.md)。

# 员工 B 第 1.2 轮审查附件

日期：2026-09-23。结论见 [员工 B 最终审查](<../员工B 最终审查.md>)，本附件用于复验证据。

## 结果

- 原版正式门禁：99 通过、0 失败、1 付费 live 忽略；fmt、Clippy、WASM 均退出 0。
- 8 个仍适用的历史探针通过，退出 0。
- 2 个新增边界探针通过，退出 0：两类工具历史在 Engine/直接 Store 均拒绝且 11 表计数不增；显式空工具数组允许续聊且原前缀保持。
- 额外 8+2 不重复计入 99。正式门禁在加入隔离探针前执行。

## 文件

| 文件 | 用途 |
| --- | --- |
| reviewed-src/ | B 的 domain、engine、store、repository 审查源码 |
| session_turns.rs | 本轮正式 17 项集成测试快照，两份探针 include 它 |
| review-source-manifest.json / review-source-recheck.json | 初始与结束复核的八个关键文件 SHA256，全部一致 |
| review-c12-comparison.json | 与已通过的 C 1.2 快照比较，重叠七个文件全部一致 |
| review-environment.log | 实际 PowerShell 7.6.5 版本与路径 |
| review-gates.json / review-test.log / review-clippy.log / review-wasm-check.log | 四项门禁结果；fmt 无输出，退出码记录在 JSON |
| review_b12_probes.rs / review-probes.log / review-probe-results.json | 原 9 个探针源码及本轮选择运行其中 8 项的命令结果 |
| review_b12_boundary_probes.rs / review-boundary-probes.log / review-boundary-result.json | 新增 2 个边界检查及实际输出 |

## 复验与排除旧过程的原因

固定副本：`./.local/temp/fufu-b12-review-5494090598`。它包含有效未提交修复，不能仅 checkout HEAD 代替。

实际复用负责人空闲独立缓存 `./.local/temp/fufu-c-review-22cad04b86\target`，没有操作员工 target。日志里的源码目录是 b12 副本，缓存名称不代表使用旧 C 源码。

先在隔离副本执行原项目 test/fmt/clippy/wasm-check。之后将本目录两份 review_b12 探针和匹配的 session_turns.rs 放在该副本 tests 下，配置项目 MSVC 环境运行：

```powershell
. .\build.ps1 -Action check
cargo test --locked --test review_b12_probes review_ -- --skip review_b11_partial_fixture_must_have_its_own_delta --test-threads=1 --nocapture
cargo test --locked --test review_b12_boundary_probes review_b12_ -- --test-threads=1 --nocapture
```

第一组日志是 8 通过、18 过滤：17 项正式测试由名字过滤，另排除一个旧过程探针。旧 `review_b11_partial_fixture_must_have_its_own_delta` 硬编码了旧的 drop Harness 步骤，作用是反驳上一版夹具，不能拿它替代已改成专属 Runtime 停止的 T9。本轮对正式 T9 的目标 Task delta、Runtime drop/join、重开和草稿断言做源码核对并实际运行通过；明确保留旧探针源码和排除原因，未冒称原 9 个全部通过。

第二组日志是 2 通过、17 过滤。探针只在隔离环境编写和执行，不合入共享正式测试，不用于凑正式通过数量。

没有修改生产实现，没有使用真实 Provider/用户运行实例，没有执行付费 live。通过记录只覆盖本次明确的应用服务范围。
