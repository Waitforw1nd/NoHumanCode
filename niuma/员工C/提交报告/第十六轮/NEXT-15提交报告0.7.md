# NEXT-15 员工C报告0.7：attempt2停于S06，持续冻结

日期：2026-09-29北京时间；下列运行时刻均为2026-09-28 UTC。员工C / GPT-6-astra；执行ID C-R16-01-20260928-E01。源码树为仓库根相对 `../nhc-c-host-setup-cli`，分支 `codex/c/host-setup-cli`。

**最终attempt2：check通过、test失败，后三门禁未执行。本次W08通过，但不代表原超时根因已解决或全套通过。没有失败后重跑、动态诊断或源码修改。**

## 固定候选与实际门禁

- HEAD：`4c9c1d8bca17de4e83e50c7893806cd351031463`。
- NoManCode tree：`f8700cfc0c5890912dacdc94f2b661eae2dc04e6`。
- Rust tree：`9fe47da3d89637f08ae436bead69817fab4d26bb`。

依据经理[放行修订01](../../../项目经理/任务/2026-09-29NEXT-15最终门禁放行修订01.md)，旧驱动保留，新建 `.local/next15-final-gates-attempt2.ps1` 仅更新三SHA；产品 `build.ps1` 未改。根目录启动原命令：`pwsh -NoProfile -File .local/next15-final-gates-attempt2.ps1 -Attempt attempt2`。每步cwd为本人树 `NoManCode/rust-app`。

| 门禁 | 实际命令 | UTC开始—结束 | 实际结果 |
|---|---|---|---|
| check | `pwsh -NoProfile -File ./build.ps1 -Action check`；`cargo check --workspace --locked` | 16:26:24.7982697Z—16:26:32.5678857Z | exit0 |
| test | `pwsh -NoProfile -File ./build.ps1 -Action test`；`cargo test --workspace --locked` | 16:26:33.3609536Z—16:28:18.633974Z | exit101，立即停止 |
| fmt | 未调用；预定 `cargo fmt --all -- --check` | 未执行 | 不记通过 |
| clippy | 未调用；预定 `cargo clippy --workspace --all-targets --locked -- -D warnings` | 未执行 | 不记通过 |
| wasm-check | 未调用；预定 `cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked` | 未执行 | 不记通过 |

外层驱动工具exit1与test实际exit101分别记录。沿用E-only、offline、debug0、incremental0、jobs2，既有工具链只读；未修改全套并发/时限，未安装、release、付费live或push。四份门禁前后154文件源码hash清单完全相同，SHA256均为 `3D4F023EEF2F279ADC5B19179EF3FD2CC5DC7BE83D1F96473A4B4A9DE32EC33E`。

## 首失败与统计边界

仅执行到peachsh lib suite：**236 passed / 1 failed / 0 ignored / 0 filtered**，237个入口，23.93秒；后续workspace/integration/doc及付费live suite未启动。6个helper入口包含在236内：`credential_input::tests::redirected_process_child`、`credential_input::windows_console::tests::isolated_console_child`、`git_worktree::tests::w05_isolated_environment_helper`、`host_configuration::tests::transaction_child`、`host_configuration_crash_tests::host_process_child`、`provider::network_tests::proxy_environment_child`。扣除helper为230/1/0，单列而不替换Cargo原数；不混入之前组件成绩。

失败为 `command_execution::tests::s06_reader_error_and_unconfirmed_cleanup_are_typed_unknown`，`src/command_execution.rs:273`，原信息 `direct command pid remains: 43204`。只读代码显示该断言新起pwsh按数值PID执行Get-Process查询，未保存原进程出生身份/句柄；原日志不能确定reader_error、cleanup_unknown、wait_error三个注入子场景中是哪一个失败，也不能确定原child存活或PID复用。稍后只读查询该PID为空，不能反推失败时状态。根因尚未确认，由经理按D历史所有权处理；C未改该模块、未按裸PID杀进程。

## 本次W08真实证据

W08在上述失败suite内实际通过。父测试PID48656；三阶段原child/restart PID分别为claim 34956/39732、created 36976/14744、registered 35088/48652。各阶段listen、ready、restart-listen均被观察到；六个原child/restart均记录kill成功及reaped（exit1，属于测试主动终止），没有把其退出码作为测试失败。三次原HTTP均记录transport error：request=true、timeout=false、connect=false；不额外推断未记录的OS错误原因。

三个保留现场各22条可解析生命周期记录，共66条；claim34文件、created45文件、registered45文件，共124文件/1,441,623字节。全部原件仍保留，并在 `.local/logs/next15-final-attempt2/w08-fixtures` 保存逐件同hash的独立字节快照。生命周期、六份child日志、listen/ready标记另有发布副本，DB/Git等完整原始内容留在本人E树并由索引追溯。原attempt1的W08超时根因仍未知，本次通过不覆盖旧失败。

## 封存与冻结回执

[证据清单](NEXT-15证据/0.7/manifest.json)含39对原始/发布文件，35对原样、4份仅发布路径相对化，原件不改；双hash已核。manifest SHA256：`81B6B7A5858B0FD7B00D9FBCD9F31608E6B8C1518F6253A96AEBB57EFA66ADF6`。原日志、各步元数据、四份源码hash、新驱动、统计和124文件快照索引均可追溯。经理已通知现有证据按原字节归档至管理提交 `37173690`；本报告不将该通知冒充本人重新全量核验。

归档脚本曾发生suite名称大小写/路径匹配错误、冻结回执括号解析错误及URL被路径检查误识别；没有因此启动产品命令。原统计保留，正确suite标签另存 `suite-summary-corrected.json`，计数不变；冻结身份已按16:31:45.6157407Z实际回执校正，发布中的既有副本逐件核验后续封。具体失败及更正见归档观察文件和manifest说明。

16:31:45.6157407Z整合用回执再次确认完整三SHA、原分支及status/index/unmerged干净，驱动已结束；源树执行路径/命令行扫描以及W08父子和S06失败共8个已知PID均无当前匹配。回执SHA256：`3B79DE3876E65CA19B862A3AE3A8A466758F08895BF10CACBA7E143E24FFAE17`。这是事后只读观察，不是原失败时进程身份或存活证明。

经理已通知普通C→D整合完成、D负责补充06；本人C树不随之修改。按最新“经理+一名员工”串行安排，本报告落盘及本人持续 **source/index clean/no-build** 回执后结束本回合。等待经理D→C新候选通知，届时才按通知核收；任何后续门禁须另行精确放行并从check重启。主仓index/档案提交/跨树merge仅经理。旧报告、attempt1和唯一diagnostic1全部保留；未自行APPROVED，任务及完整第一阶段均未宣称完成。
