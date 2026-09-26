# D-R5-01 第0.3轮真实组合定向记录

2026-09-27，员工D/GPT-6-astra，执行ID D-R5-01-20260927-E01保持。经理串行合入真实B/C组合bce914a17883378cd53aefdecf75cc51ab04abdf，Rust tree ce78b10708fe9ae2af52c85e1edc890829e654eb，D接收status空。未跨写/自行合并。

## 已运行

本人E盘NoManCode/rust-app/target，进程级CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_INCREMENTAL=0。从实际安装目录设置VSINSTALLDIR；首次dot-source原build.ps1 -Action check初始化MSVC/SDK，后续仅运行该脚本switch之前的环境初始化段（移除依赖脚本目录的Set-Location），不改build脚本。

| 命令 | 结果 | 原始日志 |
| --- | --- | --- |
| . ./build.ps1 -Action check | exit0 | d-r5-check-01.log |
| cargo test --locked --lib provider -- --nocapture | exit0；16通过/0失败 | d-r5-provider-01.log |
| cargo test --locked --test provider_network -- --nocapture | exit101；0通过/4失败 | d-r5-network-01.log |

16项中含代理隔离子进程helper顶层入口，父用例实际三次执行，不作为16个独立业务能力。低层真实redirect五状态、proxy环境同进程同loopback对照、target/model/user/purpose零连接与正对照、有界JSON、SSE截断/emit失败通过。这些通过限定bce组合低层模块，不能覆盖集成失败。

新增公开Engine集成四项全部在network_preview返回Invalid失败，未发生Provider请求。定位credential_ref生成secret:route::<id>/secret:newapi-account后调用safe_metadata_text，被通用脱敏器按secret赋值拒绝。经理已确认并派原B修N07-B04，C独立真实HTTP也命中；D未修改B或通用脱敏器，未将凭据换env绕过。

## 固定测试候选与边界

新增tests/provider_network.rs提交5063e417492469e45dbd4147c54101ed6ac20f11，Rust tree5ea9789f99ac1a3039d89375287d868d0d5719fc，status空。四项覆盖完整check授权及回执、目标/path/model/raw credential变化、账户本地保存/user变更、redirect/unknown回执重开不重发。只走公开Engine配置/preview/confirm，不提供test permit或生产bypass。

首失败执行针对新增测试未提交版；提交前仅将tempfile::tempdir改为tempdir_in("target")明确后续夹具在本树E盘，业务断言/凭据来源不变，未复跑，因此不声称5063完整测试通过。首次临时库由系统TEMP创建并由TempDir回收，日志/target均在E盘。下一步接收经理B修复真实合回后原样复跑四项；发现本人问题本人修，跨归属报告。最终五门禁仍由C唯一串行。

原日志位于本人树.local/review-evidence；发布日志只替换本人绝对工作树前缀为<D_WORKTREE>，未改退出码/内容事实。原始与发布SHA256见[清单](证据/manifest-0.3.json)。此前失败/未执行记录保留。未release/付费live/push。
