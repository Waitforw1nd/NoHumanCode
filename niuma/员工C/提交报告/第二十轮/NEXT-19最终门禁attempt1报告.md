# NEXT-19 C 最终门禁 attempt1 报告

2026-11-01，员工 C / 6.1 Terra。固定受测 `b903963b13fb0fa53f04a7585379061d3380a6d6` / NoManCode `14c89bc76165b3b38d277c5aa0ca29a97fab3492` / Rust `9d6b09f50d0925f0644f59fbc048548ec6f5dcab`，六步均 exit 0。测试通过仅说明该固定组合本次结果；NEXT-19 联合验收和主线整合由经理另行裁决，历史 N18-F01、N19-F01/F02/F03/F04 未因本次通过自动归因。

| 命令 | UTC 2026-10-31 开始 → 结束 | 退出 |
| --- | --- | --- |
| `cargo check --offline --locked --workspace` | 16:00:04 → 16:01:07 | 0 |
| `cargo build --offline --locked --bin peachsh` | 16:01:07 → 16:01:24 | 0 |
| `cargo test --offline --locked --workspace -- --nocapture` | 16:01:25 → 16:06:57 | 0 |
| `cargo fmt --all -- --check` | 16:06:57 → 16:06:59 | 0 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 16:06:59 → 16:07:19 | 0 |
| `cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown` | 16:07:20 → 16:07:47 | 0 |

北京时间为 2026-11-01 00:00–00:07；精确时间见各命令 JSON。完整 workspace 原 stdout 60 个结果块合计 **819 passed / 0 failed / 1 ignored**，含 helper 入口，不能当作独立产品场景数；`tests/live.rs` 的唯一付费真实 Provider 测试按原标记 ignored，未运行。[逐 suite 数](NEXT-19最终门禁attempt1证据/logs/workspace-suite-counts.json)按 stderr Running 与 stdout 结果块顺序配对；新 `tool_call_workflow` 1/0/0、旧 `first_user_workflow` 1/0/0，各自实际执行通过。原 lib 362/0/0。第三方 `proc-macro-error2 v2.0.1` future-incompat 提示保留，不影响本次 exit 0。

## 现场与完整性

T13 唯一原 fixture 在 `../nhc-c-tool-call-workflow/NoManCode/rust-app/.local/next19-c-b903963-final-attempt1/tmp/.tmp1fy5xs`；两次真实 Host PID 58204、16124 均 kill/wait exit 1。首次停止后 raw/query 分离，DB/WAL/SHM 均存在，原停机 metadata 分别记 SHA256 `edbee11a2e909b4413b8e2265b665c0f156aaba120e5c715a11d8fc30ba789d7`、`92b740b7c144b9083e8d7ac2e141dc5daf4ed3e6862d0592db8094b93adce9f0`、`68fab3803751b361c7e9e3cd0d7ccaf4320284d0fa91d992859cf3666f09be44`；测试重启后复核 raw hash，两个目录为独立副本。测试断言两次拒绝无对应副作用、两次批准与第三 Turn 续接、事件游标/归属和旧结果唯一，均随测试通过。经理另有[最终输入与八现场核验](../../../项目经理/审查记录/2026-11-01NEXT-19-C最终输入与八现场核验.json)。

W08 claim/created/registered 三原根 `.tmpqM9dsd`、`.tmpAYnfmd`、`.tmpAbzG3I`；原 child/restart PID 分别 59252/52300、45540/9316、59660/1848，生命周期均记录 kill/reaped exit 1，本次对应断言通过。N13 专用根 `.tmpfIRP9w` 记 ready/caller pending 及断言完成，活库 raw-observed 保留但不是一致快照。T05 before/after、旧只读、拒绝、取消、HostDenied 的六个原根分别 `nhc-t05-pf3Gel`、`nhc-t05-HOlBWN`、`.tmp1CGB9R`、`.tmp5nupoC`、`.tmpWjul8e`、`.tmpowe2Xt`，各 process metadata 均 kill ok/reaped。所有路径均在本次 `tmp/`，逐文件和读取错误见清单；未为封存打开或恢复原 DB。

Rust 受 Git 管理 142 个输入前置逐字节同 Git blob；开跑前、六步后与最终八份清单一致，原清单 SHA256 `ce4f232baefb503154989edf53501ebac0ec7601e02d4ea8745da3e9a39d8e37`。旧保留现场 1580 文件全部不变，新增外部 A10 120 文件；本次 tmp 2153 文件，读取错误均 0。E盘复用本人旧 target，独立新 tmp/log，offline/locked/jobs2/dev/test debug0/incremental0；rust-app/target 前置存在。Rust/Cargo 1.98.1、MSVC 14.43.34808、SDK 10.0.22621.0、wasm32 target 已安装。

显式 build 后 CLI 29,816,320 字节、SHA256 `ca5104f088bfb5edd1a59dd845fc6fd17693388c842a7ff14db1af0ef3b00898`；workspace test 后磁盘 CLI 30,801,920 字节、SHA256 `0d4116349514781728c26efb1032bbab0699364d91b4e5f8b97dca5a807ddd93`。两者不同，如实保留为两个时间点磁盘文件事实，不声称每个进程加载了哪一哈希。

[发布清单](NEXT-19最终门禁attempt1证据/manifest.json) SHA256 `aba894e41211057d59b7b99640dbc696042906add800a24389727aef6bbe08be`，64 份原/发布文本逐项登记路径、字节、hash 和转换并核无不符；原件在 C 树未改。结束时原受测树三 SHA、branch/status/index/unmerged 空，runner heavy=[]；测试窗口释放。下一步根据经理授权只做验收文档状态登记、提交 docs-only，并再次核 Rust tree 不变；不重跑产品。

## 验收文档登记

经理授权本报告完成后，仅更新 CLI 验收文档末尾 NEXT-19 状态；docs-only 提交 `d1b77a501654a1580f9fb2ebd0a3ae974d6ea3b8` / NoManCode `ec7b3290bb5aa66977de46a0faad3242f7b28864` / Rust `9d6b09f50d0925f0644f59fbc048548ec6f5dcab`。相对受测 `b903963b13fb0fa53f04a7585379061d3380a6d6` 仅文档变更，Rust tree 与受测完全一致；diff check 通过，提交后 status/index/unmerged 空。未对文档提交另行重跑产品；联合验收和整合仍由经理裁决。

经理复审后仅将该节标题调整为长期有效的“最终门禁记录”，新增 docs-only 提交 `734ea278d2240481305df08b9187d37252f4b5db` / NoManCode `b02d06979681a7c5ee45093b871046996c5f5fb2` / Rust `9d6b09f50d0925f0644f59fbc048548ec6f5dcab`；相对受测仍只有文档变更，Rust tree 不变，diff check 通过，status/index/unmerged 空；未重跑产品。
