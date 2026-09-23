> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../../路径与可移植性.md)。

# 员工 C 第 1.2 轮复审附件

日期：2026-09-23。结论：C 第一轮现有 HTTP/SSE 范围通过，见 [最终审查](<../员工C 最终审查.md>)。

## 实际结果

- 四项正式门禁均退出 0：99 测试通过、1 付费 live 忽略；fmt、Clippy、WASM 通过。fmt 无输出，结果记录于 review-gates.json。
- 正式 HTTP 为 11 项通过，含已强制晚等待的 H7。
- 原 7 项审查探针原样复跑，7 项通过、退出 0，未计入 99。
- 当前 B 的 session_turns 为 17 项，T9 通过；与 C 报告中较早的 16 项中间态不同。本附件不构成 B 的功能验收。

## 文件用途

| 文件 | 内容 |
| --- | --- |
| reviewed-server.rs / reviewed-http_contract.rs | 被审查的 C 正式源码，无负责人修复 |
| review-http-diff.txt | 与第 1.1 轮 HTTP 测试对比，仅 H7 三处变更；含 Git 换行提示，是比对记录 |
| b-integration-session_turns.rs | 本次全量通过时的 B 测试快照，用于说明与员工报告的版本差异 |
| review-source-manifest.json / review-source-recheck.json | C/B 七个关键文件的初始 SHA256 与结束复核，全部一致 |
| review-environment.log | 真实 PowerShell 7.6.5 路径与版本 |
| review-gates.json / review-test.log / review-clippy.log / review-wasm-check.log | 四项正式门禁的实际结果 |
| review_c12_probes.rs / review-probes.log / review-probe-results.json | 原 7 项审查反例的独立夹具、日志和退出码 |

固定源码副本：`./.local/temp/fufu-c12-review-d94f190c6f`。

编译缓存复用负责人当时空闲的 `./.local/temp/fufu-c-review-22cad04b86\target`。缓存路径带旧轮次不代表使用旧源码；实际编译目录见日志。没有操作员工 target。

## 复验

在隔离副本使用真实 PowerShell 7.6.5 执行项目 build.ps1 的 test/fmt/clippy/wasm-check。仅检出 HEAD 会遗漏已验收但未提交的修改，需保留正确工作区基线。

四项正式门禁完成后，将本目录的 review_c12_probes.rs 复制到隔离副本 tests 下，配置项目 MSVC 环境并运行：

```powershell
. .\build.ps1 -Action check
cargo test --locked --test review_c12_probes review_ -- --test-threads=1 --nocapture
```

文件内历史 HTTP 夹具测试由 filter 排除，仅运行 7 个负责人审查探针。不要将整份重复夹具合入正式测试。最后遗留的 H7 已由当前正式 HTTP 测试验证，本轮没有重复引入上一轮故意保留旧通知的失败对照。

不执行付费 live，不调用生产 Provider。此附件用于验证，未改变共享实现或正式测试。
