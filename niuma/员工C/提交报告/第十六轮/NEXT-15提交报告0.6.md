# NEXT-15 员工C报告0.6：W08取证修补静态交付

更新日期：2026-09-29北京时间；执行时刻按2026-09-28 UTC记录。员工C / GPT-6-astra；原执行ID C-R16-01-20260928-E01。源码树为仓库根相对 `../nhc-c-host-setup-cli`，分支 `codex/c/host-setup-cli`。源码提交执行人为C，主仓档案提交与跨树整合人为经理。

**本报告仅证明补充05范围内取证代码已提交、静态格式及范围检查通过。新候选未编译、未动态运行。原W08超时根因仍未知，未宣称产品超时已修复、最终门禁通过或APPROVED。**

## 授权与版本绑定

依据[冻结补充05](../../../项目经理/任务/2026-09-29NEXT-15冻结补充05-W08失败取证.md)，先封存[报告0.5](NEXT-15提交报告0.5.md)及原失败/唯一诊断，再只修 `NoManCode/rust-app/src/server.rs` 内原 `#[cfg(test)] mod worktree_http_crash_tests`。

| 版本 | 完整HEAD | NoManCode tree | Rust tree |
|---|---|---|---|
| 原失败及单次诊断基线 | fa9623785573a8f44decd0368fa73a4e267f4ed6 | 2ec3a9f19c9a0f01721581d3cd0a7752187ae522 | 1fcf1e19dcda993dd1778a37e50e613e971d5153 |
| 首静态取证候选 | d2a0b6878a1e83bd3d48c2884fd4cea649ae4e88 | 0002c7fd00c5384acb3166049e43a50a56ce5d3f | 8148741e2cbd1863518c56d84e787c273266e5ec |
| 本次交付候选 | 4c9c1d8bca17de4e83e50c7893806cd351031463 | f8700cfc0c5890912dacdc94f2b661eae2dc04e6 | 9fe47da3d89637f08ae436bead69817fab4d26bb |

本候选 `src/server.rs` 工作区文件SHA256：`DCE96D03D00E6960D36BE3C7085FC456C60163C1991B390668C787C2E1565AB5`。首候选同文件原SHA256：`75D48334CBAA68F04780BB183653A3B9DAA06B24A4D2311796F4B314F77CEF59`。Git tree与文件字节hash分别记录，不混用。

首候选已采用完整JSON行先缓冲再append，满足经理随后提出的格式化碎片交错风险修正要求。接续静态复核进一步发现取证持久化失败提示仍用 `eprintln!`，stderr写失败可能panic；于是仅追加忽略stderr写入错误的单点保护，形成本候选。经理随后确认该修订属于补充05，并明确暂停绑定d2a0b687的旧放行。旧精确放行到达时源文件已改，本人没有把旧SHA授权套用于新内容，没有启动attempt2。

## 实际改动与保持的边界

- 三phase临时目录使用 `TempDir::keep()` 保留成功与失败现场，输出phase及路径，保存原child stdout/stderr。新增 `lifecycle.jsonl` 记录时间戳、phase、通知类型、PID与实际生命周期观察。
- `ChildGuard` 仍只操作实际持有的原 `Child`，记录spawn、kill请求/结果、exit/reaped及清理期限；不按猜测PID终止无关进程。
- `wait_file` 区分listen通知与ready事务停点，在原10秒边界内观察真实child退出及原HTTP handle是否完成，不消费该handle、不重发请求。超时信息包含phase、通知名与PID。
- 原HTTP仍只发一次：收到headers立即记status，仅检查声明长度不超过4096的body并输出白名单既有 `worktree_*` code；transport仅记分类布尔值。没有输出原body、消息、headers、URL、token/key。收到HTTP响应后即使诊断body读取失败，仍返回 `Ok(status)`，不把提前响应伪装成响应丢失。
- JSON先整体格式化成包含换行的缓冲，再交给append `write_all`；避免多次格式化片段写入，不据此宣称跨进程事务或任意底层部分写入下的原子性。持久化失败只尝试固定stderr提示，忽略该提示的写入错误，避免I/O失败路径引起二次panic。
- 原pending至kill后的 `request.await.unwrap().is_err()` 与业务/DB/工作树断言均保持。相对fa962378模块外文本相同，28个原断言入口数量相同且diff未删除原断言行；未改变生产server、B/D模块、Host/HTTP真实装配或恢复语义。

静态对比的完整Duration表达式顺序前后一致：5s、10ms、5s、10ms、12s、10s、10ms、20s、10ms、60s、10s、10s。10秒等待、12秒HTTP与20秒hook均未放大或取消。未减少/忽略测试、修改全套并发或添加重试至绿。

## 实际静态命令与退出码

所有命令cwd为本人源树 `NoManCode/rust-app`；所有shell显式E目录，TMP/TEMP为本人树 `.local/tmp`，target为 `.local/target`，原始日志全部在E。既有 `rustfmt 1.9.0-stable (48a229ceae 2026-09-01)` 只读，`RUSTUP_AUTO_INSTALL=0`，未安装/升级。

| 实际原命令 | 结果 |
|---|---|
| `rustfmt --edition 2024 --config style_edition=2024,skip_children=true src/server.rs` | 三批静态阶段均exit0 |
| `rustfmt --check --edition 2024 --config style_edition=2024,skip_children=true src/server.rs` | 三批静态阶段均exit0 |
| `git diff --check` | 三批静态阶段均exit0 |
| `git add -- src/server.rs` | 两次候选仅指定本人文件暂存；末次记录exit0 |
| `git commit -m 'test(worktree): retain W08 failure evidence and observe child lifecycle'` | exit0，生成d2a0b687完整SHA见上表 |
| `git commit -m 'test(worktree): ignore diagnostic stderr write failures'` | exit0，生成4c9c1d8完整SHA见上表 |

首静态核验时刻16:15:43.9329496Z；完整行append首候选回执16:16:49.3914008Z。末次格式至提交为16:21:28.8509512Z—16:21:29.7515828Z，完整冻结回执16:21:30.2943023Z。三批原件分开保留，不用末次覆盖首批。

两次工具层JavaScript引号/转义错误均发生在shell调用之前：一次为此前读取tempfile时，另一次为本次静态脚本 `SyntaxError: Unexpected identifier 'n'`。都没有对应产品命令或shell退出码；修正传输后实际静态命令结果如上。不是产品测试失败或动态重跑。

**本窗口未执行：编译/type-check、既有binary诊断、任何动态测试，以及check→test→fmt→clippy→wasm-check最终门禁。局部rustfmt检查不等于全workspace fmt门禁通过。** 没有release、付费live、push、跨树merge或主仓index操作。

## 原始与发布证据

发布清单：[NEXT-15证据/0.6/manifest.json](NEXT-15证据/0.6/manifest.json)。共22对原始/发布文件，逐件记录相对路径、长度和双SHA256，全部字节原样发布。manifest SHA256：`2310BE82B4A9EB1C4E08C8CD7537F44FC4F22A88D2AB4619D6B98330C5537BA0`。

原件位于本人树 `.local/logs/next15-w08-static`、`.local/logs/next15-w08-static-final` 与 `.local/logs/next15-w08-static-stderr`。包含三批格式/diff日志、原diff、首静态检查、两个完整候选回执与提交日志、末次独立stderr差异/累计W08差异/staged diff、逐命令UTC与退出码、工具传输失败观察。

0.5原manifest仍为 `E144DF08257E3A70D263572BE7C9A2EBEB3EF14393DDAE3222DE69D4DE10B9C7`，其36对证据及143个诊断现场版本索引保留。旧报告与失败没有重写。原最终attempt1仍是check0/test101、lib236/1/0（含6helper，扣除230/1/0），后三门禁未执行；唯一既有binary诊断1/0/0、236 filtered只证明该次三个phase通过，原超时未复现。

## 当前冻结与下一步

本人已核4c9c1d8完整三SHA、分支、status/index/unmerged均空、无本树构建/Host/test进程，持续 **source/index clean/no-build**。不再追加源码；经理负责精确review与新候选最终放行。

下一第一操作：经理重新绑定完整候选并明确放行后，再由本人实核并在全新attempt目录从check开始，严格check→test→fmt→clippy→wasm-check串行；任一步失败停止后续并保全增强W08现场。旧驱动仍绑定fa962378，不能复用其版本绑定。原根因未确定，整个第一阶段仍未完成。
