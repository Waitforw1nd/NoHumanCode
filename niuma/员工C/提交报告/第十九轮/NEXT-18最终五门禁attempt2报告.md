# NEXT-18 最终五门禁 attempt2 报告

2026-10-31，员工 C / 6.1 Terra，C-R19。按经理明确窗口，本人固定 `codex/c/runtime-management-interface`，HEAD `7dc734b0832dcd888a4e67fda1d666deb990cd15` / NoManCode `9057a7999fc1c8a34322b6254a31fb946ec82bb3` / Rust `535566761693515f6087a1a306385487c580518d`。直接 Cargo、offline/locked、debug0、incremental0、jobs2；原工作树内独立 `.local/logs/next18-final-attempt2/`、`.local/tmp-next18-final-attempt2/`，复用 `.local/target/`。逐步真实命令、UTC、环境、三 SHA 和退出码见[metadata](NEXT-18证据/最终attempt2/meta.json)。

| 步骤 | 实际结果 |
| --- | --- |
| CLI 前置 `cargo build --offline --locked --bin peachsh` | exit 0；真实 CLI 29,572,608 字节，SHA256 `54D2C0F047905E99EF06A3968518E4183F989FD7C964B0B0436AAE2CD36E9ABA`；不算五门禁 |
| `cargo check --offline --locked --workspace --all-targets` | exit 0 |
| `cargo test --offline --locked --workspace -- --nocapture` | exit 0 |
| `cargo fmt --all -- --check` | exit 0，原 fmt.log 0 字节 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | exit 0 |
| `cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown` | exit 0 |

session 75572 已自然结束 exit 0。原 workspace 测试输出共58个结果块，原始合计 **768 passed / 0 failed / 1 ignored / 0 filtered**，包括 helper，不叠加组件结果；唯一 ignored 为需要付费 key 的 `xpeach_native_stream_and_coding_tool`，未运行付费 live。`first_user_workflow` 本轮实际 ok。原失败 `workspace_crash::write_crash_after_file_before_finish_commit_is_unknown` 本轮实际 ok，整个 `workspace_crash` 为6/0/0（含两helper）。这次通过不解释 attempt1 失败原因，也不自行关闭 N18-F01 或称产品已修复。check/test/Clippy日志中的第三方 `proc-macro-error2 v2.0.1` future-incompat 提示按原文保留；WASM仅证明该目标可编译。

## 现场与证据

13个 runtime 迁移/意图阶段（9迁移、4意图）共234文件；3个 W08 阶段共124文件；5个 A10 fixture 根共60文件及15个日志明确phase快照共120文件；两次 write crash 根各16文件、共32文件。原文件与本轮 `.local/logs/next18-final-attempt2/preserved-fixtures/` 逐字节副本逐一核长度与SHA256，详细路径、phase、文件、kill/wait及恢复进程见[现场索引](NEXT-18证据/最终attempt2/fixture-index.json)，原索引SHA256 `954407A676BAAC3ABD39EDB70C754B24F60749B69B7913EB251CEC31F721B3BB`。未打开或恢复原始 crash 数据库；用户与经理可据独立 queryable/recovery 副本审证。13个 runtime 阶段各记录真实 kill/wait 和两次不同 recovery PID，现场保存存在的原 DB/WAL/SHM 与 queryable。

两次 write 的 `process.json` 均记录 `phase=target-boundary`、kill=killed、wait exit1、reaped=true，原 DB/WAL/SHM 与 raw 三件套逐字节一致；均保留 queryable、observed、独立 recovery、子进程日志及实际文件。file-before-finish：root `workspace-write-crash-a1ERWK`、task `f6d2853b-37f7-42a4-b29b-2068ed762122`、child PID 35788，observed 记录 change prepared、approval claimed。finished-after-commit：root `workspace-write-crash-9TphQQ`、task `3666b02c-3f81-4e74-9b9a-6c3045abd744`、child PID 44156，observed 记录 change finished、approval finished。两者的 recovery 为独立复制数据库后由测试所在进程重新打开；不宣称另有未记录的 recovery PID。本轮测试对恢复行为的断言实际通过。attempt1 的失败目录未记录且无法归属，旧失败及现场不足仍保留，不能拿本轮现场冒充旧次。

六命令前后十二份源码清单均168项、SHA256 `20540E2E4D47C888D046F63BB54CB08C70F681BFB18CDB59FFB30EE1993C1E14`。原 test.log SHA256 `1B1DF4C124F107C56969CA1B8C349ADF4B8025931AB0C40A8D0CFDF0F22987CE`。原日志、清单、驱动、统计与现场索引共22对原/发布文件逐对核对长度和SHA256，7份仅路径转换；[manifest](NEXT-18证据/最终attempt2/manifest.json) SHA256 `3FBF7AAF4CD5AED42DC6E00BC4FC12C12BA16C43F60271036805E19EF761BD49`。

2026-10-31T08:00:51Z 本人再次核完整三 SHA、分支、status/index/unmerged 空，命令行关联本人树无进程；已释放重型窗口并持续 source/index clean/no-build。交经理独立审证与裁决，未自行合 main；NEXT-18 尚未联合验收，整个项目与Desktop仍未完成。
