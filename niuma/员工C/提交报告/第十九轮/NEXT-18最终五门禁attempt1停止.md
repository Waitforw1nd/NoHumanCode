# NEXT-18 最终五门禁 attempt1 停止

2026-10-31，员工 C / 6.1 Terra，C-R19。固定 HEAD `7fce5b2e187a1147f4fcc784b5d07580eb6afe2c`、NoManCode `da5787dfff470eda857fff8c7934432ba5fc8cc3`、Rust `b5846dcd4c4a8584d2b36fe0a2c95e688c2fbf83`。原件 `.local/logs/next18-final-attempt1/`、独立现场 `.local/tmp-next18-final-attempt1/`、target `.local/target/` 均相对本人工作树；驱动 `.local/next18-final-attempt1.ps1`。完整命令、UTC、版本、环境和源码清单见[原发布映射](NEXT-18证据/最终attempt1/manifest.json)及其中 metadata。

| 顺序 | 实际结果 |
| --- | --- |
| 预先 build --offline --locked --bin peachsh | exit 0；CLI SHA256 `FBA8321635708D71068052F87A9C04F75EC255B6252C9B8CA52ECBD99C0CC758`；不是五门禁之一 |
| check --offline --locked --workspace --all-targets | exit 0；保留第三方 `proc-macro-error2 v2.0.1` future-incompat 提示 |
| test --offline --locked --workspace -- --nocapture | exit 101；workspace_crash 5 passed / 1 failed / 0 ignored，遇首失败停止 |
| fmt / workspace all-targets clippy / wasm-check | 未执行，不能记为通过 |

session 1225 结束 exit 1。到失败为止原日志含 49 个 `test result` 结果块，原始合计 741 passed / 1 failed / 1 ignored / 0 filtered，含 helper、未叠加组件；这是**未完整运行的 workspace 日志统计**，不是完整 workspace 通过数。唯一 ignored 是需要付费 key 的 `xpeach_native_stream_and_coding_tool`，未执行付费调用。本次实际 `first_user_workflow` 测试输出 ok，temporary fixture 未证明被保留。普通 panic hook 输出按各测试上下文判断，不笼统归类。

## 失败与现场边界

唯一失败 `workspace_crash::write_crash_after_file_before_finish_commit_is_unknown`，原 `tests/workspace_crash.rs:567` 记录 20 秒未到 finish 窗口：文件字节是 `before-crash`，`state=Some("failed")`，child stderr 空。源码明确该 state 是 `workspace_changes.state`，不能称 Task 状态为 failed。失败根因未证实，不改断言、timeout 或产品，不自行重跑。

该测试使用未保留的 `tempfile::tempdir`，日志未打印目录或 task ID；本人对本次独立 TMP 顶层仅按测试同时具备的 `child-task-id` 与 `write-child.stderr` 标记只读搜索，未发现可归属目录。故无法从独立 snapshot 确认失败 Task、审批、调用与变更的细节；不能将其他数据库当作此现场。已向经理明确此不足，交指定 owner 定位。

## 可归属恢复证据

`--nocapture` 的记录及本轮独立 TMP 支持索引本次 13 个 runtime 迁移/意图阶段：9 个 migration、4 个 intent，共 234 个文件。每个保留根及文件均保留原件，复制到本次 `.local/logs/next18-final-attempt1/preserved-fixtures/runtime/` 后以长度/SHA256逐文件核对；包含存在的 raw DB/WAL/SHM、signal、kill/wait、queryable 快照及两次不同 recovery PID。原 raw 数据库未打开；manager 已另对 queryable 快照作独立审证。

W08 的 claim/created/registered 三个日志明确路径，分别 34/45/45 文件、合计124个，原件及逐字节核对副本保留。A10 本次5个带 `crash-child.stdout.log` 的现场根共60文件，按原日志五条 `a10-fixture` 的目录/phase 明确映射；日志另有 `a10-evidence` 的15个阶段快照根共120文件，按原日志phase精确索引。上述所有拷贝均与原件长度和SHA256匹配；详见[现场索引](NEXT-18证据/最终attempt1/fixture-index.json)，其原件 SHA256 `5EB7C90225158B7AF926E88D90ECCDA9E577A9DCEE2EB867D32D401937554E54`。现场复制不等于为未打印路径的 workspace_crash 失败找到证据。

build/check/test 前后六份清单各168个跟踪文件，SHA256 `E4CD832FA945582873311F3109F715EDB7F3DF5DEA951505E8C8F983D2773CD8`。原 test.log SHA256 `E07D547AF6342879B1CF5658A83D315DB30EAE3937329F5A6B8455C6C75A7C9D`。日志、清单、驱动及派生索引共14对原/发布长度和SHA256逐一核验，其中5份仅路径替换；v2 manifest SHA256 `7559B7BE7228D55A47E0EA7AEE6C48A2D1521536C4BA726013B4AD682FD6A3BB`。初版未使用已记录的 A10 根/phase 映射，现按日志修正，初版索引及 manifest 均保留。所有旧 attempt 及现场保留。

2026-10-31T07:23:25Z 本人核三 SHA、branch/status/index/unmerged 空、命令行关联本人树进程无匹配；已停止并释放重型窗口，继续 source/index clean/no-build。下一步将结果和证据交经理，等待 D 原 owner 的失败定位及新候选/精确放行；最终门禁未通过，NEXT-18 未验收或合 main。
