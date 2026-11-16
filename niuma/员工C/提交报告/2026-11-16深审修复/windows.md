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

夹具要求临时目录支持生成实际 8.3 名称；没有别名时明确失败，不能无声跳过并当作通过。不修改磁盘全局 8.3 设置。本次 Windows 原语检查使用宿主默认临时目录，确实生成了上述别名。

边界：仍是现有检查后使用的文件策略，不是持句柄文件系统沙箱；不声称关闭并发替换或任意硬链接别名问题。按原请求拼写检查的非通配写入范围语义保持。

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
