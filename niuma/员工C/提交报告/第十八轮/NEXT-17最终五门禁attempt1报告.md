# NEXT-17 最终五门禁 attempt1 报告

2026-10-31，员工 C / 6.1 Terra，C-R18-01-20261031-E01。经理在双方冻结基础上将完整B/C/D组合交本人树并精确放行，本人核收后执行；**五门禁实际均 exit0、外层 session 97598 exit0**。此报告提交经理独立核验证据和联合验收，不自行 APPROVED；整个阶段一、Desktop和全项目均未宣称完成。

受测本人树 `../nhc-c-agent-runtime-interface`、branch `codex/c/agent-runtime-interface`，HEAD `a95e9923e7ba293dba964670c3fc4d98fbd9357a`、NoManCode tree `906328dea19bc45b0a29c8279b931144b08144cb`、Rust tree `7e9f77907c5df4712d2689dd22532579d3e01e15`。每步前后及跨步骤十份161跟踪源码文件清单相同，SHA256 `7F257CE498D0BA3D730604BAC1B133A0F4015B81965632C95FBC568D8FA981DC`；全部三SHA/status/index/unmerged固定、干净。

## 原命令与结果

工作目录为本人树 `NoManCode/rust-app`；每步通过 `pwsh -NoProfile -File ./build.ps1 -Action <action>` 执行。时间UTC，均为2026-10-31。

| 门禁 | Cargo命令 | 开始 | 结束 | exit |
| --- | --- | --- | --- | --- |
| check | `cargo check --workspace --locked` | 04:38:24.7811469 | 04:39:16.4598377 | 0 |
| test | `cargo test --workspace --locked` | 04:39:17.2037872 | 04:43:42.7572638 | 0 |
| fmt | `cargo fmt --all -- --check` | 04:43:43.4773118 | 04:43:45.1348467 | 0 |
| clippy | `cargo clippy --workspace --all-targets --locked -- -D warnings` | 04:43:45.8171322 | 04:44:06.4796520 | 0 |
| wasm-check | `cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked` | 04:44:07.1543097 | 04:44:32.7982933 | 0 |

原始 test.log **55个结果块合计718 passed / 0 failed / 1 ignored / 0 filtered**；不扣helper、不叠加组件成绩。唯一 ignored `xpeach_native_stream_and_coding_tool` 需要付费密钥和付费调用，未执行。完整lib含D `agent_runtime_tests`，公开诊断组件测试及 `first_user_workflow` 1/0/0 均在本次workspace内；[逐suite派生统计](NEXT-17证据/最终attempt1/suite-summary.json)辅助核算，原日志是依据。

沿用已安装MSVC/SDK与WASM target、`CARGO_NET_OFFLINE=true`、`RUSTUP_AUTO_INSTALL=0`、debug0、incremental0、jobs2，默认测试并发及原时限不改；工程、target、TMP/TEMP/log均在本人树，预备 `rust-app/target`。fmt.log真实0字节。test和Clippy日志均保留Cargo对第三方 `proc-macro-error2 v2.0.1` 未来Rust不兼容的提示；Clippy `-D warnings` 仍实际exit0，该提示未被误报为Clippy失败，也不是无任何警告。未安装升级、执行release/付费live、push或清理。

## 现场、证据与冻结

W08本次测试ok，父PID 43336，claim/created/registered各22条生命周期，原child与重启child分别为30832/42828、40824/43872、21468/15020；六者均有kill成功和reaped（exit1为测试主动终止），drop_reaped不重复计数。三处共124个现场文件原件留本人树 `.local/tmp`，逐字节复制到 `.local/logs/next17-final-attempt1/w08-fixtures/` 并核对；旧W08超时与之前证据不覆盖。

本次A10按 `crash-child.stdout.log` 现场文件特征定位5个保留目录、共60文件，原件留 `.local/tmp`，相对路径/长度/SHA256见[现场索引](NEXT-17证据/最终attempt1/a10-file-index.json)；workspace成功测试输出默认capture，日志没有逐phase目录映射，不擅自猜测。`first_user_workflow` 本次通过，其成功输出同样被capture，临时夹具未设置持久保留，日志没有可归属目录；不声称已保留其完整现场。D独立组件既有现场另按D报告核验，不与本次统计或索引混算。

原始日志根 `.local/logs/next17-final-attempt1/`、驱动 `.local/next17-final-gates-attempt1.ps1` 均相对本人树；原 test.log SHA256 `10365C5E19E8828D4BC466FC2C783344FD801B931C2F8C68F5F593925FF68EBD`。[发布清单](NEXT-17证据/最终attempt1/manifest.json)含29对原件/发布副本的字节数和SHA256，已逐对读取核验，其中5份仅相对化工作树/工具链路径，其余原样；清单SHA256 `FE69531FF693A3CA1905D6A7544A6B6AFFA42440EFABFB5FCA33A11F2805D8C3`。W08完整二进制现场在本人树，发布包含生命周期、子日志和索引；A10仅索引，suite-summary为派生文件，不计入29对。

04:48:38.4545824Z 本人再次核完整三SHA、status/index/unmerged空，命令行关联本人树的进程扫描无匹配；session 97598已自然结束，持续source/index clean/no-build。上述时点扫描不推断其他时刻身份。未改产品、schema、权限或凭据；下一第一步等待经理独立核验、联合验收及按受测tree整合，主仓index与跨树merge仅经理。
