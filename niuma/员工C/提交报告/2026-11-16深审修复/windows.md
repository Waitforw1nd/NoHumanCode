# 2026-11-16 Windows 文件边界与 Shell 输出首个候选

更新人：本轮员工 C。状态：已实现、静态检查通过，Rust 回归尚未编译或运行，待主线程串行验证与整合。

## 基线与所有权

- 独立分支：`codex/audit-20261116/windows`；独立工作树：`../nhc-audit-20261116-windows`。
- 开始 HEAD：`bd31edeb80ab6801af6987ee50e719d14f457935`，index/工作区干净。
- 用户本轮明确恢复开发并指定新分工。本轮没有接管旧 C 的 47 项草稿，没有修改旧 B/C 树、主仓产品或主仓身份。
- 已读根/源码 AGENTS、开发守则及主仓员工 C 身份顶部；主仓身份由经理同步。本轮只提交下列两个源码文件和本报告。
- 未运行 Cargo、构建、产品、真实进程测试或旧事故诊断；历史 unknown 保留。没有启动子代理。

## W16-F01：8.3 短名绕过敏感名称检查

真实触发路径：`workspace::resolve` 原先仅对请求拼写调用 `forbidden`，随后 canonicalize 只检查根目录包含关系。短文件名或短目录名仍指向根内同一对象，因此不会触发原限制；`read_file`、`write_file`/`change_target` 都经过该入口。

已执行仅含合成文件的 Windows 原语检查：临时目录中写入固定 `synthetic-non-secret`，通过 `GetShortPathNameW`/`GetLongPathNameW` 观察到：

| 合成对象 | 实际短名叶子 |
| --- | --- |
| `.credentials.yaml` | `CREDEN~1.YAM` |
| `.env.production` | `ENV~1.PRO` |
| `.ssh` 目录 | `SSH~1` |
| `node_modules` 目录 | `NODE_M~1` |
| `ordinary-directory` | `ORDINA~1` |

原始词法策略拒绝长名而接受以上敏感短名。该检查不是运行 Rust 产品的漏洞复现；它验证了反例所需的真实 Windows 别名关系。未访问真实秘密；合成对象在 finally 中按确定路径逐个删除。

修复：每一级已存在组件完成原有 reparse/目录策略检查后，对 canonicalize 得到的根内相对路径再次检查敏感名称，并继续使用真实长路径构造后续组件。因此保护目录短名后的不存在文件也被拒绝；普通目录短名后的新文件返回长目录下的预期目标。非 NotFound 的元数据错误明确拒绝，不当作路径不存在。原写入范围检查保留，不新增授权。

回归源码：

- `workspace::tests::windows_short_names_cannot_hide_protected_files`：真实短名、读写拒绝、合成内容不变；含 credentials、env、数据库叶子。
- `workspace::tests::windows_short_directories_cannot_hide_protected_ancestors`：真实目录短名，目录本身、已有文件及多级不存在目标在读写模式均拒绝，不产生副产物。
- `workspace::tests::windows_allowed_aliases_preserve_existing_and_missing_targets`：普通文件/目录别名正控、创建目标正控、范围外写拒绝、change_target 长名投影。

初版夹具要求临时目录自动生成实际 8.3 名称；没有别名时明确失败。本次初始 Windows 原语检查使用宿主默认临时目录，确实生成了上述别名。主线程随后在 E 盘验证发现自动生成不可用，修订见本文后续复核段；没有跳过失败或修改磁盘全局 8.3 设置。

边界：仍是现有检查后使用的文件策略，不是持句柄文件系统沙箱；不声称关闭并发替换或任意硬链接别名问题。初版保留按请求拼写检查非通配写入范围，后续复核另补真实路径范围校验，见下文。

## W16-F02：64 KiB 捕获上限被误当作 EOF

真实触发路径：`command_execution::collect` 的两个 reader 原来使用 `.take(65536).read_to_end`，达到上限就返回并丢弃管道。写入更多输出的已批准命令可能遇到关闭管道，且该 reader 完成不证明真实 EOF；继承管道的后代更可在 reader 提前完成后继续存在。

修复：抽出私有 `read_output`，每次读取固定 8 KiB，持续排空到实际 EOF，只保留每个流前 65,536 字节。上限之后的真实 I/O 错误继续传播到既有 cleanup/unknown 路径。stdout/stderr 仍并发读取；返回 JSON 字段和每流保留上限不变，没有新依赖或新模块。

回归源码：

- `command_execution::tests::output_reader_drains_beyond_limit_and_keeps_only_prefix`：三倍上限的源被完全消费，返回严格为原前缀。
- `command_execution::tests::output_reader_waits_for_real_eof_after_limit`：达到上限但 writer 尚存时必须保持 pending；实际关闭后有界完成。只使用内存 duplex，无 OS 子进程或后台 task。
- `command_execution::tests::output_reader_reports_errors_after_capture_limit`：超过上限后注入读取错误，必须返回错误。
- `command_execution::tests::s16_large_output_keeps_both_pipes_open_until_command_finishes`：真实已批准 Shell 执行路径输出双流各 128 KiB，退出码为 0、末尾 marker 存在、各保留 64 KiB；复用原进程句柄核验及 reader 计数。命令本身不创建后代，隐藏窗口，10 秒执行上限和原 5 秒清理上限。仅准备源码，未运行。

## 失败、取消、恢复与未关闭边界

- 文件拒绝发生在读取/创建之前；不存在目标仍交给既有调用者处理。原链接、越根、目录策略与写入范围校验保留。
- reader 不再因保留上限假 EOF；真实 EOF、I/O 错误及既有超时决定收集结束。未新增永久等待、park 或故障无限循环。
- **完整 Shell 进程树寿命未关闭。** 当前 launcher 仍仅持直接 child；后代主动关闭/重定向管道后可继续运行，直接 child 退出加真实 EOF 不能证明树空。现有超时清理仍只 kill/wait 直接 child，也不能据此声称所有后代已结束。
- **与员工 B 关闭协议的接口边界：** 当前 collector 没有接收 Engine 的取消令牌，执行预算为 1..600 秒，加 5 秒 cleanup。B 的有界关闭等待到期只能报告未收束/unknown；不能把取消请求、task abort 或 runtime 退出当作命令及后代终态证据。
- 未导入旧 B23。它的第16例 unknown 不随本候选变化；本候选不改变 schema、恢复 claim、权限、凭据或持久化格式。

最小下一实施方案：单独冻结并验证创建时 Job 归属及原进程/reader 持有模块；Shell/Git 应共享它，而不是 spawn 后补挂 Job 或另建各自监督框架。成功证明应同时要求原进程 signal、Job active=0、两 reader EOF/join；取消/超时保留实际 owner 至可证明收束，无法证明则粘性 unknown。该步骤需协调 Engine/工具服务传入取消与监督所有权，超出本次限定写集，不能由本补丁冒称完成。

## 验证与整合选择

已执行：Windows 合成别名原语检查退出 0；两个文件直接 `rustfmt --edition 2024` 与 `rustfmt --check`；`git diff --check`。未执行：任何 Cargo 编译、Rust 测试、产品、真实进程测试、广泛回归。格式成功不代表编译成功。

主线程配置现有 MSVC/输出目录后，在 `NoManCode/rust-app` 串行执行以下选择；本报告没有运行这些命令：

```powershell
cargo test --offline --locked --lib workspace::tests::windows_ -- --test-threads=1
cargo test --offline --locked --lib command_execution::tests::output_reader_ -- --test-threads=1
cargo test --offline --locked --lib workspace::tests::file_boundaries -- --exact --test-threads=1
```

另开明确真实进程测试步骤，仅运行：

```powershell
cargo test --offline --locked --lib command_execution::tests::s16_large_output_keeps_both_pipes_open_until_command_finishes -- --exact --test-threads=1 --nocapture
```

相关旧 collector 期限/错误测试可由主线程在同一受控窗口择需运行；不要把旧有后代未知夹具自动纳入无进程检查。建议先整合并验证本独立补丁，再单独处理进程树组件及统一关闭接线。

明确文件集：

- `NoManCode/rust-app/src/workspace.rs`
- `NoManCode/rust-app/src/command_execution.rs`
- `niuma/员工C/提交报告/2026-11-16深审修复/windows.md`

提交 SHA 由本报告同次提交后的回执提供，避免文档自引用哈希。无需改 `lib.rs`。

## 2026-11-16 串行验证后的最小修订

初次提交为 `ba285d4b83bc9154064bb525a3709016c63e0141`。本段后的提交只修改 `workspace.rs` 与本报告，不修改命令 collector 或新增进程树实现。

### E 盘夹具真实失败与修正

已只读核实主线程 `../nhc-audit-20261116-integration/NoManCode/rust-app/.local/audit-20261116/windows-workspace-tests.log`：Rust 编译成功，workspace 单测 2 通过、3 失败；三个新测试都在自动短名不存在的前置断言失败，未触及目标拒绝/允许断言。该结果不是产品修复通过。

随后在本树 E 盘 `.local/audit-20261116` 的随机合成目录进行 Windows 原语验证。只为自身 `.credentials.yaml` 文件及 `.ssh` 目录调用 CreateFileW/SetFileShortNameW，分别指定 `NHCRED~1.YAM`/`NHSSH~1`。两对象均 `set_ok=true`、`set_error=0`、GetShortPathNameW 返回指定短名且别名路径存在。句柄已关闭，合成文件/目录按原名逐项删除。未修改全局自动 8.3 设置、未调整权限、未启动产品/真实进程测试。

夹具改为通过 `SetFileShortNameW` 给自身合成对象显式设置唯一合法短名，再断言 GetShortPathNameW 返回该名称、短长路径 canonical identity 一致。设置失败仍明确失败；不静默跳过、不替换为字符串模拟，也不移往 C 盘。新候选 Rust 回归尚未重跑，由主线程继续 E 盘串行验证。

### W16-F03：真实路径也必须落在声明写范围内

反例：真实目录 `ordinary-long-directory` 存在别名 `NHC00001`。任务声明写范围 `nhc00001`，请求 `NHC00001/existing.txt` 或 `NHC00001/missing/nested.txt` 会通过原词法前缀检查，但目标真实相对路径属于 `ordinary-long-directory`。Engine::scope_conflicts 和项目范围上限按词法比较，两个名称会被当成不同范围；不能靠运行时展开 alias 扩张声明命名空间。

最小修复：保留原请求范围检查，并对 resolve 最终返回的真实相对路径再执行同一范围判定。仅短别名声明现在拒绝写入；`*` 仍允许普通别名，声明 `src` 时 `src` 内部普通目录/文件别名仍允许，未存在目标沿已解析祖先继续判定。读取语义不变。

新增有意义回归：

- `workspace::tests::windows_alias_only_scope_cannot_authorize_another_namespace`：先断言词法范围确实不重叠且旧条件会通过，再核真实目标拒绝、缺失父目录未创建、已有内容未改、读取仍正常。
- `workspace::tests::windows_alias_below_declared_long_scope_remains_writable`：声明长范围内的文件与目录别名正控，以及已存在/多级不存在目标。

兼容变化明确：仅靠短别名定义的写范围须改成实际长路径范围；没有在此修改 Engine/项目配置存储或自动重写用户配置。并发文件名替换的检查后使用窗口仍属于原边界。

### 现有非 ignored 命令测试过滤审查

`command_execution::tests` 共 10 个非 ignored 测试：3 个纯内存 reader 测试、1 个新增大输出测试、6 个旧 s06 测试。未发现这些测试或继承管道所用 prepare 正常路径中的显式无限 park：prepare 外层 15 秒；两个脚本各自有 60 秒停止条件；collector 为执行期限+5秒 cleanup；Job 查询等待 5 秒。

但计时器不构成真实进程/底层同步 I/O 的全部收束证明。本仓锁定 Tokio 1.53.1；已读本机同版本源码 `src/process/windows.rs` 的 ChildStdio 使用 `Blocking<ArcFile>`，`src/io/blocking.rs` 在后台执行真实 read。外层 reader task abort/join 不证明已经启动的后台阻塞 read 结束；运行时退出会等待 started blocking task。

首轮精确排除 `command_execution::tests::s06_deadline_covers_inherited_pipe_after_direct_parent_exit`：它调用 `process_test_fixture::prepare` 启动实际后代，后代持 stdout/stderr，collector 超时后才由测试调用 fixture.cleanup 发 release/终止 Job。准备失败/清理失败时 Job::drop 只记录错误并退出，没有独立确认与异常接续；若底层读仍未结束，异步 timeout 不会形成进程级硬上限。该风险未在本轮动态复现，不能描述成必现无限挂起；此用例需另行原对象监督窗口。

推荐首轮无进程选择：

```powershell
cargo test --offline --locked --lib workspace::tests:: -- --test-threads=1
cargo test --offline --locked --lib command_execution::tests::output_reader_ -- --test-threads=1
```

在主线程明确的真实进程窗口内，下列完整名称逐个用 `cargo test --offline --locked --lib <名称> -- --exact --test-threads=1 --nocapture`；不要笼统选择整个 command_execution 模块：

| 完整测试名 | 夹具与边界 |
| --- | --- |
| `command_execution::tests::s16_large_output_keeps_both_pipes_open_until_command_finishes` | 一个隐藏 PowerShell，无后代；10秒执行+5秒清理，核原进程与reader |
| `command_execution::tests::s06_original_handle_rejects_live_and_query_failure_accepts_exit` | 一个隐藏 PowerShell；显式 kill 后5秒 wait，控制结果在清理后断言 |
| `command_execution::tests::s06_real_timeout_reaps_child_and_readers_without_false_unknown` | 一个隐藏 PowerShell，1秒执行+5秒清理；慢启动可能导致 ready 断言失败，但不含后代循环 |
| `command_execution::tests::s06_execute_two_second_deadline_starts_before_spawn` | 一个隐藏 PowerShell，2秒执行+5秒清理 |
| `command_execution::tests::s06_reader_error_and_unconfirmed_cleanup_are_typed_unknown` | 三个串行隐藏 PowerShell；每个1秒执行+5秒清理，unknown 为注入，最终核原进程退出 |
| `command_execution::tests::s06_task_scoped_unknown_hook_is_once_guarded_and_reaps_real_spawn` | 四个串行隐藏 PowerShell；默认每次30秒执行+5秒清理，其中一次注入unknown；不创建后代 |

以上 allowlist 仍需外部原进程监督来处理 OS 清理失败；不是对任意 OS 故障的无条件无遗留保证。不要把 `process_test_fixture::process_fixture_failure_controls_reap_job` 等独立夹具负控意外带入当前首轮；它们不属于上述 exact 选择。

本次只运行合成文件原语及 rustfmt/diff 静态检查；未运行 Cargo、产品、真实进程、卷配置命令。历史 unknown 不改判。
