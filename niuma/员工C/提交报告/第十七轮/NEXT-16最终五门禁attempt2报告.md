# NEXT-16 最终五门禁 attempt2 报告

2026-10-31，员工 C / 6.1 Terra，C-R17-01-20261031-E01。本人依[第二候选放行](../../审查记录/第十七轮/2026-10-31最终五门禁第二候选放行.md)核收后，从check完整串行执行；**五门禁均 exit0，外层 session 60669 exit0**。本报告提交经理独立证据核验与联合验收，不能自行 APPROVED，也不代表完整阶段一、Desktop或全项目完成。

受测本人树 `../nhc-c-first-user-workflow`、branch `codex/c/first-user-workflow`，HEAD `79fe075676236a0085451fbe303a2e7b3f794377`、NoManCode tree `9cf2fd7a0d2e30c5cb51ce63c5ecc981a0e1d37e`、Rust tree `65b3da024f5e9d83ba7aaf7aed3bd86cfc27a57b`。每步前后、跨门禁十份158跟踪源码文件清单一致，SHA256 `EEF15B6E1C4D774802739F9F01FB69BB70A5E80AF6C70C09B41D4CD545099172`；每次三SHA/status/index/unmerged均符合固定候选。

## 原命令与真实结果

在本人树 `NoManCode/rust-app` 逐项通过 `pwsh -NoProfile -File ./build.ps1 -Action <action>` 执行；时间UTC，均为2026-10-31。

| 门禁 | Cargo命令 | 开始 | 结束 | exit |
| --- | --- | --- | --- | --- |
| check | `cargo check --workspace --locked` | 02:59:26.6298116 | 02:59:32.5696011 | 0 |
| test | `cargo test --workspace --locked` | 02:59:33.3949589 | 03:03:20.6867280 | 0 |
| fmt | `cargo fmt --all -- --check` | 03:03:21.5621701 | 03:03:23.2811307 | 0 |
| clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` | 03:03:24.0229593 | 03:03:43.9849260 | 0 |
| wasm-check | `cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked` | 03:03:44.7420139 | 03:04:10.6104199 | 0 |

原始 test.log **54 个结果块合计 687 passed / 0 failed / 1 ignored / 0 filtered**；此为原始总数，不扣 helper、不叠加组件。唯一 ignored `xpeach_native_stream_and_coding_tool` 需要付费密钥且会产生付费调用，本次未运行。原始统计包含 helper 入口，具体以日志为准；[逐suite派生统计](NEXT-16证据/最终attempt2/suite-summary.json)仅辅助核算。首轮失败的 `restore_http_tests::partial_restore_committed_barrier_http_reopen_no_retry` 本次 ok，`first_user_workflow` 本次 1/0/0；旧attempt1失败与记录仍保留。

使用既有MSVC/SDK与已安装WASM target，`CARGO_NET_OFFLINE=true`、`RUSTUP_AUTO_INSTALL=0`、debug0、incremental0、jobs2；默认测试并发及时限不变，工程/target/TMP/TEMP/log均在本人树。未安装升级、运行release/付费live或push；fmt.log真实0字节。

## 真实现场与证据

W08本次 ok。三个阶段生命周期各22条，父PID 15752；claim原进程11156/重启45320、created 17384/45988、registered 35344/42696，六个原child均有kill成功与reaped记录（exit1为测试主动终止）；drop_reaped不是额外回收。三个现场共124文件，已复制至本人树 `.local/logs/next16-final-attempt2/w08-fixtures/` 并逐文件字节核对，原件留在 `.local/tmp`。本次W08通过不覆盖旧超时或推断历史根因。

原始日志根 `.local/logs/next16-final-attempt2/`，新驱动 `.local/next16-final-gates-attempt2.ps1`，均相对本人树；原 test.log SHA256 `E6DBB6284694DE8E0DE5169B43E919E0E2081364947454FCCAEC776A609C5F1F`。[发布清单](NEXT-16证据/最终attempt2/manifest.json)记录28对原件/发布副本字节数和SHA256，已逐对读取核验，其中5份仅相对化工作树/工具链路径，其余原样；清单 SHA256 `E8F19CFD7FA3B312FD9BB2E9267C3BC2A72F48FD0366773E2DAFFDA8841911EA`。完整二进制现场在本人树，发布包括三阶段生命周期、child日志及124文件索引；suite-summary 为派生文件，不计入28对。

03:05:17.3028360Z 本人再次核完整三SHA、status/index/unmerged空，命令行关联本人树的进程扫描无匹配；session 60669已结束。本人明确持续 source/index clean/no-build，已回经理，供独立审核和按受测tree整合。旧attempt1的失败、原证据与现场全部保留；此时点进程观察不推断其他运行。
