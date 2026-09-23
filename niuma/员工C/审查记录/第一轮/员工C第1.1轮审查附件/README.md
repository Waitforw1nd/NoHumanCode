> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../../路径与可移植性.md)。

# 员工 C 第 1.1 轮复审附件

日期：2026-09-23。结论见 [review 1.1](<../员工C review 1.1.md>)。本目录用于复验证据，不是正式测试入口。

## 结果

- 正式源码四项门禁全部退出 0：94 项通过、1 个付费 live 忽略，其中 HTTP 11 项通过。fmt 无输出，以 review-gates.json 的退出码为证。
- 原 7 个审查探针全部通过，C-R1～C-R4 闭合。
- H7 调度探针合计 1 通过、1 失败：原 notify_waiters 在先通知后等待时超时；仅改 notify_one 的对照完整通过。没有改共享正式测试。

## 文件

| 文件 | 用途 |
| --- | --- |
| reviewed-server.rs / reviewed-http_contract.rs | 当前被审查的原版 C 源码，不含审查改动 |
| review-source-manifest.json / review-source-recheck.json | 初始 SHA256 及审查完成后的工作区比对，7 个文件全部一致 |
| review-environment.log | 实际 PowerShell 版本和程序路径 |
| review-gates.json / review-test.log / review-clippy.log / review-wasm-check.log | 原版门禁结果；探针在门禁结束后加入 |
| review_c11_probes.rs / review-probes.log | 原第一轮的完整独立夹具与 7 个负责人探针，本轮原样复跑 |
| review_c11_h7_schedule.rs / review-h7-schedule.log | 当前完整 HTTP 夹具，加两个复制的 H7 调度探针；正式 11 项通过 filter 排除 |
| review-probe-results.json | 两组探针的真实 cargo 退出码，分别 0 与 101 |

## 复验

本次固定源码副本：`./.local/temp/fufu-c11-review-e80a349693`。

`CARGO_TARGET_DIR` 指向负责人先前的空闲独立缓存 `./.local/temp/fufu-c-review-22cad04b86\target`；名字含旧轮次只代表缓存，日志中的实际编译源码目录是上述 c11 副本。没有使用或清理员工共享 target。

在保留有效未提交源码的隔离副本中，将两个 `review_c11_*.rs` 文件放到其 tests 目录，用项目脚本配置 MSVC 环境后执行：

```powershell
. .\build.ps1 -Action check
cargo test --locked --test review_c11_probes review_ -- --test-threads=1 --nocapture
cargo test --locked --test review_c11_h7_schedule review_h7_ -- --test-threads=1 --nocapture
```

第二条 cargo 命令在本轮原版中预期退出 101：这正是记录的测试同步反例。它不是正式门禁命令。不要将整份重复夹具直接复制进正式测试或用这个额外失败掩盖正式门禁已通过的事实。

真实脚本 shell：`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`，版本 7.6.5。

## H7 反例为何有效

两个探针都在 POST 返回 Run 后、创建 entered 等待前，有界 yield 到 calls 为 1。单线程运行时中 calls 加一和 started 通知之间没有 await，故此时通知已发生，Provider 停在 release gate。这个顺序本来就可在等待 HTTP 响应/JSON 时发生。

原版探针接下来等待未保存的 notify_waiters，3 秒超时。对照仅将 started 通知换成 notify_one，仍保留同样的强制晚等待，以及消费帧、断开、busy、放行完成、一次调用、重连游标的全套断言，因此支持最小修法而不减弱验收目标。

本附件只证明握手调度存在竞态，没有证明生产断线功能失败。C 下一轮应将针对性交错纳入小范围正式回归，由员工编写和测试后交回负责人复审。
