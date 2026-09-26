# 员工C：Diff/Checkpoint HTTP与CLI 第1.0轮报告

2026-09-27，员工C / GPT-6-astra。任务 `C-R7-01 / NEXT-06`，执行标识 `C-R7-01-20260927-N06`；首次本机登记 `2026-09-27T01:34:10.1964203+08:00`，精确消息送达秒数未采集。统一契约[经理NEXT-06 v1.0](../../../项目经理/任务/2026-09-27NEXT-06真实Diff与最小Checkpoint契约.md)。本报告只认领C实现/验证，B/D领域贡献保持。

## 固定输入与Git

基线 `7a4a001d4296a26086d5196997ae80ac88e1c22b`；独立树 `../nhc-c-review-cli/`，分支 `codex/c/review-cli`。C唯一提交本人授权文件，经理独占依赖合并与共享index。初始WIP `1c8b292b48076298cc70ff50a6f8057ea5345d8a`，经理合B API为 `1000158c3e821a66b4a501c0924555bc5aede9ef`；C夹具/严格JSON候选 `ca36aabc582967ed83974506ae2c1b7d7d72430b`；经理合B/D完整依赖为 `39d0bbdeb8c6cfde028ce14799460696fdf8c562`。

C语义补证候选 `296dfea7c7413d5435df535225a041de598aa6fb`；工程fmt修复为 `69ec3c21499a21680e84537e07ec79f17168b089`。B仅移动store生产items修Clippy后经理再次合并，最终受测候选 `9f990648df3308dc7235b028123e5532a35a00f8`，Rust tree `b549de2e18d39c8e69e6607b9f430974895ee4b9`。C修改仅 src/cli.rs、src/server.rs、新 tests/review_cli.rs、tests/checkpoint_http.rs、旧 tests/http_contract.rs schema8→9预期；没有修改main、B/D实现、Cargo、构建脚本、旧档案或共享index。用户打断后沿用原模型/ID/源码，未重开执行者。

## 用户可见行为与边界

CLI新增 `task diff <id> --path <relative> --view staged|unstaged|head`（默认head）、`task checkpoint <id> --key <key>`、`checkpoint get <id>`、`checkpoint restore <id>`。真实diff委托D/Engine；checkpoint委托B，不在传输层复制manifest、持久化或恢复算法。task_before明确表示Task首次工具写入前字节，不是目录快照。

HTTP新增POST tasks/{id}/git-diff、POST tasks/{id}/checkpoints、GET checkpoints/{id}、POST checkpoints/{id}/restore。POST空body只接受{}；Diff只接受对象，重复/未知字段拒绝。沿用Content-Type/body限制、Host/Origin/token和安全头。创建201/同key回放200；三typed错误安全穷尽映射，未知500、不输出内部路径/SQL、retryable=false；恢复partial/unknown409保留安全receipt。

CLI沿用loopback、no_proxy/no_redirect、1MiB有界输出、token解码后防回显。写POST不自动重试；差异POST是只读请求，坏响应不误标写结果未知。checkpoint restore先读取稳定checkpoint→task身份，再核成功或409 receipt属于该Task；partial/unknown exit1带receipt，参数错误exit2。固定wire投影拒绝响应换路径/对象身份，无apply命令。N06-C01已修复：反斜杠归一为/后比较响应路径，保留真实文件名大小写，不把scope的小写身份键替换Git请求路径。

## 验收矩阵与证据边界

| 条目 | C实际证据 |
| --- | --- |
| T01 | 真CLI子进程→真Host→生产审批write_file修改+新建；真实Git三视图分别显示HEAD/index/工作文件不同字节；反斜杠输入/default head；create/get/replay/冲突→kill/wait重启同data-dir身份稳定→restore最早before/删除新建→再次重启同receipt不覆盖外改→同Session新Turn成功。HEAD/index/其他文件字节未变；新Task changes为空仍能diff外部untracked。 |
| T02 | HTTP每个拒绝请求前后业务表计数/Provider次数不变；鉴权/Host/Origin、坏JSON/数组对象/unknown field/超限/Content-Type、缺失/合并/重复key、坏ID、404/409/Corrupt500；static typed错误单测覆盖全部新业务变体和旧Workspace NotFound/Active。真实unknown及Windows partial CLI exit1携receipt，重复恢复不重做。受控恶意响应含坏JSON/1MiB+/转义token/redirect/断流，对新写命令结果unknown，对只读diff安全失败，每次精确单POST；binary/too_large/missing/unchanged/redacted text状态可见，换路径响应拒绝。 |
| K05支持 | 真实敏感before经批准写后创建checkpoint，HTTP201/200/GET及run SSE无before正文/密文/内部root；manifest篡改使get/create replay/restore安全500且文件/事件数无新增。schema6/7/8→9完整保全与伪约束主证归B，C标准全量已执行其固定测试，不把作者归属改为C。 |
| K06支持 | 真Host新checkpoint HTTP入口，在文件已恢复且SQLite outcome事务仍active的确定性长查询窗口，读取claimed/restore_id后杀Host并wait真实退出；重启unknown/原ID，checkpoint可查、外改后重复两入口不覆盖、新Turn拒绝且Provider无调用。pre-claim及claim后内部真子进程由B主证。完整恢复后的真实重启稳定receipt由T01同时覆盖。 |
| G01–G05 | D真实Git模块主证；C仅上述真实HTTP/CLI三视图与外部文件/路径绑定贯通，不冒称独立重做D所有恶意Git及race矩阵。 |
| T03 | 经理固定候选后C唯一串行五标准动作；结果见后表。 |

Mock Provider只运行本地确定性响应，不宣称付费模型实跑。崩溃测试实际kill+wait进程，不以drop Arc或中断HTTP future冒充进程终止。CLI信号等旧功能由回归保持，本轮不额外宣称系统Ctrl+C注入。已有AP/AH/W/Turn/CLI断言未削弱，旧HTTP仅改schema预期。

## 实跑、失败与未运行

Rust/Cargo1.98.1、PowerShell7.6.5；运行时发现MSVC安装并为调用进程设置VSINSTALLDIR，标准build.ps1未修改。全部构建为D盘本人树target，CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_INCREMENTAL=0。C从未创建/使用C盘target；磁盘阻塞时只做源码，B当时磁盘失败属于B证据。

- 组合首次与补证后的两轮定向：checkpoint_http均3/3、review_cli均6/6，exit0。
- server严格对象/typed错误单测2/2，exit0；C bin与两新测试Clippy exit0。
- 已知夹具SQL误用events.run_id在实际跑测前静态发现并修为JOIN tasks，无伪造失败日志。首次编辑脚本因Python默认GBK读取UTF-8失败，未改源码；改进程PYTHONUTF8后成功，属于编辑工具失败非产品测试。
- 一次同shell调用：第一server-tests成功后脚本改变cwd，第二相对runner路径找不到，Clippy未启动；之后正确独立调用Clippy0。该shell exit1保留说明，没有补造当时原始命令日志/精确开始时间。
- 第一轮固定296dfea：check0、test0（345通过/0失败/1付费忽略）、fmt1；Clippy/wasm未执行。C两测试格式来自总根默认工具链，按工程stable工具链只修导入排序/断言换行，没有降低断言。
- 第二轮固定69ec3c2：check0、test0（345/0/1）、fmt0、Clippy101；wasm未执行。B新增store生产items位于mod tests之后触发items_after_test_module；C未越权改B文件，经理协调B移动原block，未加allow或关闭lint。经理合B纯移动913565a形成最终9f99064，重新完整运行五门禁。两轮test均无失败，fmt/Clippy失败不能计入测试失败数，也不能称那两轮全部通过。
- 未运行release、付费live、push；旧树与九档案保留。五门禁中的live测试按原规则忽略。

原始日志/命令账保存在本人树 `.local/C-R7-evidence/`；发布副本在[C-R7证据](C-R7证据/)，转换只做路径相对化/BOM与换行，双hash见[manifest](C-R7证据/manifest.json)。无输出fmt如果未生成文件，只引用命令账，不补造日志。

## 最终五门禁与交付

固定 `9f990648df3308dc7235b028123e5532a35a00f8`，Rust tree `b549de2e18d39c8e69e6607b9f430974895ee4b9`，五项全exit0；最终test **345通过/0失败/1付费live忽略**，不加之前两轮或定向数。所有命令从本人树 `NoManCode/rust-app` 执行。

| 命令 | 开始—结束（+08:00） | 退出码 | 发布证据 |
| --- | --- | ---: | --- |
| `pwsh -NoProfile -File build.ps1 -Action check` | 2026-09-27T02:25:49.9524336+08:00 — 2026-09-27T02:25:53.7740587+08:00 | 0 | [gate-complete-check.log](C-R7证据/gate-complete-check.log) |
| `pwsh -NoProfile -File build.ps1 -Action test` | 2026-09-27T02:25:53.8260045+08:00 — 2026-09-27T02:27:15.2506445+08:00 | 0 | [gate-complete-test.log](C-R7证据/gate-complete-test.log) |
| `pwsh -NoProfile -File build.ps1 -Action fmt` | 2026-09-27T02:27:15.2930580+08:00 — 2026-09-27T02:27:16.7173792+08:00 | 0 | [命令账](C-R7证据/commands.jsonl)（无输出、无日志文件） |
| `pwsh -NoProfile -File build.ps1 -Action clippy` | 2026-09-27T02:27:16.7852019+08:00 — 2026-09-27T02:27:22.2039867+08:00 | 0 | [gate-complete-clippy.log](C-R7证据/gate-complete-clippy.log) |
| `pwsh -NoProfile -File build.ps1 -Action wasm-check` | 2026-09-27T02:27:22.2458561+08:00 — 2026-09-27T02:27:48.4857254+08:00 | 0 | [gate-complete-wasm-check.log](C-R7证据/gate-complete-wasm-check.log) |

完整command ledger绑定每轮HEAD，最终每项另绑定[五门禁汇总](C-R7证据/final-gates.json)同一tree；C五个文件字节SHA256与仓库/源码tree见[源码清单](C-R7证据/final-source.json)。原始fmt失败日志存在；两次成功fmt无输出、未创建日志文件，仅保留实际时间/退出码，未补造空原始日志。上游proc-macro-error2未来兼容警告保留，非本轮lint失败。

源码和本人档案交付后冻结；当前不是经理验收、main合并或发布声明。下一条操作是等待经理固定review，只按明确返工恢复本人范围写入。旧树保留，未release/live/推送。
