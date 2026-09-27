# 员工C：9c996a6组件专项与Clippy停止报告1.2

2026-09-28，C/GPT-6-astra，C-R14-01-20260927-E01。经理合入B02-04/D04-05与diagnose第四参后，实际核对HEAD9c996a6a00e1657afd649ab6a70da41322ce8a99、NoManCode5e895df00b58db647434c81654087c4c8fd769b6、Rust552e4e19762a301223cc4b32d46cce0d9274cc95，status/index空，按组件授权继续。所有三次测试meta的前后HEAD/tree/status相同；没有源码修改/新提交，不操作共享index，最终五门禁未执行。

| 组件 | 精确命令 | 实际结果 |
|---|---|---|
| W09/W01/W02 | cargo test --locked --test worktree_cli --test worktree_http | exit0；CLI8/0/0、HTTP2/0/0 |
| W08 | cargo test --locked --lib server::worktree_http_crash_tests::w08_real_router_process_kill_wait_at_three_durable_boundaries -- --exact --nocapture | exit0；1/0/0、185filtered；父测试包含三实际场景，无空helper |
| 限定Clippy | cargo clippy --locked --lib --bin peachsh --test worktree_cli --test worktree_http -- -D warnings | cargo exit101；外壳exit1，不混淆 |

新组合W02整项通过：clean诊断与正常审批写文件后的dirty诊断都是PresentMatching；独立Project授权/写根、原Session新Turn根和旧Task绑定保持、全部重复读取/原key回放/真实CLI status尾部均已执行。关闭先前N13-D04在C真实HTTP场景的失败，但不替代经理D05专项review。CLI malformed/body流错误/duplicate/字段不一致/输出失败/只读POST失败，HTTP DTO超大/重复/security/跨source也整项通过。

W08父日志明确打印三个durable hook、原进程kill/wait、新进程验证与kill/wait。claim/created重开unknown、registered重开succeeded；无额外worker，runs/network_calls计数0。该测试未观测Provider socket，日志简写“no Provider dispatch”以DB计数/worker证据解释，不扩大为OS级网络隔离。

Clippy实际首个阻塞：src/git_worktree.rs:84 ProjectGitObservation枚举NotGit无数据，Repository(RepositorySnapshot)至少240 bytes，触发large_enum_variant。D所有权，已交经理，不跨写、不增加allow。构建已结束，C无构建且源码冻结，待经理D修新完整候选后必要复核；本报告不宣称组件全绿。

[第三批6对原始/发布SHA256 manifest](C-R14证据/第三批专项/manifest.json)已生成并逐对核验，原件保留，发布仅相对化工作树/MSVC路径；加前两批累计18对。每份meta含命令、UTC、完整输入与实际exit。

[W09/W02覆盖映射](NEXT-13W09-W02覆盖映射.md)记录具体测试断言及旧Task封存/Checkpoint Conflict现有回归。三项旧Engine回归确有5c8f221 test日志ok，本次尚未重跑；不以旧结果冒充新组合结果，不为计数增镜像测试。
