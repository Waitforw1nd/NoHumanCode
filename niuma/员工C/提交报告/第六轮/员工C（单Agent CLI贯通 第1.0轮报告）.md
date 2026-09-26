# 员工C：单Agent CLI贯通 第1.0轮报告

2026-09-27，员工 C / GPT-6-astra。任务 `C-R6-01 / NEXT-05`，执行标识 `C-R6-01-20260927-000811`；首次本机读取登记 `2026-09-27T00:11:42.5771437+08:00`，未采集消息精确送达秒数。依据[经理冻结契约 v1.0](../../../项目经理/任务/2026-09-27NEXT-05单Agent贯通契约.md)。旧 C-R5 已验收完成，本报告不重新认领其贡献。

## 固定输入、Git与边界

- 基线 `5cf74319ea3b37af9c27821a6eaac944241473e4`；独立工作树 `../nhc-c-cli-workflow/`，分支 `codex/c/cli-workflow`。C 只暂存/提交自己授权源码，经理负责依赖整合及共享 index。
- CLI骨架 `0cd0f309f3af746cddf966b6238a49e438ce6f15`；经理接 B API 得到 `29a36277d4ac6b4930ebcfac6d967aca4458ef05`；C server检查点 `efa242f00a59a65e583d873939899f8729e97cbd`；独立 CLI 阶段 `bc0237a1f771d814e7f03866773db289b36c23ce`。
- 经理合入 B `8df22e3664b6d1a773661a2b3879ee2b284ec853`（包含 D `3d00f121b0c66124cbb2afc2d6fdf0d1cf504c10`），组合 `9b76898f1a4f2162d53af5487d58b9191b575417`。C 最终候选 `1a3f3e0840331d54e3419b5826b845d3667feed0`，提交后 `git status --short` 空。
- C 实际源码修改仅 `NoManCode/rust-app/src/cli.rs`、`src/main.rs`、`src/server.rs`、`tests/turn_http.rs`、新增 `tests/cli_workflow.rs`。main 仅 CLI 参数校验提示，server 仅 context及typed错误映射；没有代写 B/D 产品实现、改依赖/schema/构建脚本。
- 服务中断后沿用同代理/执行标识，经理备份后核 server 在途 SHA-256 `C197CF1471C10CF9D9E7EEA0BD210D4E8B97043274D19670B50A7B0891A7B8C3` 一致继续，没有重置或覆盖。实际模型始终记录为本轮 GPT-6-astra；旧轮模型贡献不改写。

## 最终行为

新增 project list、run start/list/status/context/events、session status/turns/send/events、task resume/cancel/changes/restore；既有四个审批命令保持。start 固定当前 Host workspace 的单 Agent/Task chat，工具及命令默认关闭，scope 无默认星号；scope/allow-commands 需要 tools。新 Turn 显式传入 agent/after-turn/key，不猜前序、不自动改 key 重发；resume 继续旧任务。

GET `/api/runs/{id}/context` 直接调用 B 的 `Store::run_context`，安全映射 NotFound/Unmapped/CorruptState 为404/409/500；SQLite瞬时错误保留 retryability。UnresolvedEffects 为409 conflict、retryable=false。状态和恢复使用安全投影/typed receipt，不输出 messages、before_blob；run start 直接输出创建receipt和run_id，不在创建后强依赖第二次context查询。

客户端固定loopback/no_proxy/no_redirect；普通响应上限1MiB，普通请求短超时，每次写先bootstrap，token不持久化、不回显、写操作不自动重试。不可验证的写响应、断流和超限返回 operation_result_unknown，并提供对应查询/原key核对说明。restore partial/unknown以exit1保留typed receipt/status/restored及逐路径事实。

SSE使用独立client，连接2秒、响应头4秒、读取停滞30秒，无4秒整流超时；单帧缓存1MiB。支持跨chunk UTF-8、CRLF、注释、多行data和多帧，id严格非负十进制i64且匹配event.seq；事件完整输出flush后才推进游标。JSON模式NDJSON；limit exit0，EOF/流内error/坏帧exit1并输出最后游标。Ctrl+C代码只结束观察，不调用cancel；本轮信号注入未单独实跑，观察断开不取消由真实Host测试证明。

## N01–N14证据

| 编号 | 实跑证据与范围 |
| --- | --- |
| N01 | C `n01_n02_n04_real_cli_host_tool_restore_new_turn_and_restart` 与context分类测试：真CLI子进程→真Host，project发现、Run/Session不同ID、真实Agent/TurnTask、CLI目录无DB；不存在404、旧未映射409不造身份、损坏500、错误Origin403。 |
| N02 | C同一真实流程：生产runtime read_file、拒绝write_file不落盘、批准write_file改字节、批准本地PowerShell测试命令；显式断言工具结果exit_code=0和实际marker字节，再completed。审批数量及mock Provider请求数固定，原key重放不增加调用。 |
| N03 | C流程覆盖同Session/Agent新Turn/新Task及新审批、旧审批不增加；精确权限继承、重复call ID拒绝及旧调用来源由[B第1.0报告](<../../../员工B/提交报告/第四轮/员工B（工具会话与生产网关 第1.0轮报告）.md>)主证。最终组合全量同时执行B测试，不将其作者归属改为C。 |
| N04 | C真实complete restore→新Task批准写，before_digest与恢复前原始字节摘要相等，原工具结果保留且后续模型请求有Host恢复事实。旧Task封存/二次restore逐字节由B主证。 |
| N05 | C真实restore outcome提交失败产生unknown，真Host重启后CLI新key send返回conflict/retryable=false、context不变、Provider无新增调用；其他pending/claimed/change/partial类别及已提交key回放由B主证。 |
| N06 | C HTTP旧结构安全目标保留：当前自有消息非法role/tool_calls仍400、闭合但无来源event的伪造调用400、篡改继承prefix500；每例请求前快照且零Provider增量。事务候选权限/Route/workspace及错误审批来源由B主证。 |
| N07 | C原turn_http同key/不同key并发和生命周期回归；工具会话scope、生产send/restore屏障由B主证。 |
| N08 | [D第1.0报告](<../../../员工D/提交报告/第三轮/员工D（builtin工具生产装配 第1.0轮报告）.md>)主证真实resolver binding/payload、替代payload、类型/版本/权限/活动态反例；C不将模块夹具宣称为CLI信号。 |
| N09 | B生产Engine撤销/真实claim屏障与D运行时锁/lease三分支主证；C完整流程确实经过合入后的生产runtime。 |
| N10 | C真CLI/Host changes/restore。unknown：真实写后通过临时DB trigger让outcome提交失败，稳定unknown receipt，重启与再次restore不覆盖后来外部编辑。Windows partial：两次真实审批写、首文件已恢复且DB写事务busy后独占第二文件，真实CLI回执first complete/second unknown且exit1；重复receipt不重做。不以直接写终态冒充真实恢复。 |
| N11 | C真实run/session SSE游标手动续读；受控流逐字节UTF-8/CRLF/comments/multiline、坏JSON/ID/UTF8/seq/超限/error/EOF保留最后cursor；5秒keepalive超过普通4秒请求timeout成功且忽略代理。实际Host观察limit断开后任务仍running。Ctrl+C单独系统信号未注入。 |
| N12 | C实际Host子进程kill并wait退出，同data-dir启动：completed工具Turn的身份/审批/changes可查且第三Turn继续；中断任务恢复为interrupted、原key不再执行、仅显式resume有新调用；unknown恢复receipt不重做。mock Provider独立进程内服务保持，仅Host真重启。 |
| N13 | 原CLI11项回归覆盖代理/重定向/转义token等；新增workflow参数exit2、五类写坏响应、截断/1MiB超限/转义token回显均unknown且单次POST、不落token。原key回放真实执行；幂等冲突的精确HTTP/Store规则由旧Turn及B测试承担，CLI不自动修正代码已核。 |
| N14 | CLI9项、原CLI11项、turn_http17项定向通过；固定组合五标准门禁结果见下表。AP/AH/W/Turn旧覆盖保留；付费live仍忽略，未release/推送。 |

## 实跑与失败保留

所有Rust命令在本独立工作树 `NoManCode/rust-app` 执行；stable Rust/Cargo1.98.1、PowerShell7.6.5，进程VSINSTALLDIR发现本机MSVC后使用原build.ps1设置环境，本工作树独立target。原始字节日志在 `.local/C-R6-evidence/`。每条已登记命令起止/退出码见[C命令账](C-R6证据/commands.jsonl)，缺失的早期精确开始时间不倒填。

| 阶段/命令 | 结果 |
| --- | --- |
| 初始及server检查点check；context mapping单测 | check exit0；映射1/1、exit0。 |
| 初始N13定向 | 1/1，exit0。 |
| 独立workflow首轮 | 4通过2失败，exit101：损坏夹具被FK提前阻断；安全字符串误用scrub压缩正常多行输出。明确临时fixture关闭FK及改用标量redact_persisted比较后6/6。 |
| 旧CLI回归；后续独立workflow | 原CLI11/11；workflow8/8、1 filtered。当时依赖B的完整工具流程未运行，保留当时事实。 |
| CLI阶段clippy/fmt | 首次clippy collapsible_if exit101，修正后exit0；fmt exit0。 |
| 组合完整workflow首次及三次诊断 | 首次8通过1失败、exit101；后续三次诊断均exit101。批准命令exit0/无stderr但标记文件未产生，实际工具历史/回执保存；用独立临时PowerShell调用复现位置参数绑定问题。改Set-Content为明确-LiteralPath/-Value/-ErrorAction Stop，保留exit_code与文件双断言后通过。临时复现仅工具输出，没有独立日志或补造时间。 |
| 组合workflow+turn_http首次 | CLI9/9，HTTP16/17、exit101；原坏结构改到了继承前缀，新来源规则先500而原预期400。按规则分别验证当前坏结构400和继承损坏500，保留两安全目标。 |
| 修正后workflow+turn_http | CLI9/9、HTTP17/17，exit0；最终追加继承prefix反例后N09再1/1、exit0。 |
| 固定候选五门禁 | check/test/fmt/clippy/wasm-check全exit0；全量303通过、0失败、1付费live忽略。 |

## 交付状态

当前源码 `1a3f3e0840331d54e3419b5826b845d3667feed0` 干净冻结，Rust tree `ba569b4e5c4365d5e71a3075ecc7fafe48373bfa`。C按经理指定为唯一联合验证人串行完成五门禁，期间无源码修改，结束status为空、无后台编译。上游proc-macro-error2 future-incompat提示保留，不是本轮lint失败。正式报告与证据交经理review；未宣称经理验收、main整合或发布。未执行release/付费live/推送。下一步第一条操作是等待经理固定review，只有明确返工项才恢复限定写入。

## 最终五门禁记录

全部绑定完整候选 `1a3f3e0840331d54e3419b5826b845d3667feed0`，本地时间为 +08:00。

| 命令 | 开始—结束 | 退出码 | 发布证据 |
| --- | --- | ---: | --- |
| `build.ps1 -Action check` | 2026-09-27T01:03:47.8118558+08:00 — 2026-09-27T01:03:48.8172602+08:00 | 0 | [gate-check.log](C-R6证据/gate-check.log) |
| `build.ps1 -Action test` | 2026-09-27T01:03:48.8306748+08:00 — 2026-09-27T01:05:16.0360517+08:00 | 0 | [gate-test.log](C-R6证据/gate-test.log) |
| `build.ps1 -Action fmt` | 2026-09-27T01:05:16.0373047+08:00 — 2026-09-27T01:05:17.4253123+08:00 | 0 | [命令账](C-R6证据/commands.jsonl)（无输出，未产生原始fmt日志） |
| `build.ps1 -Action clippy` | 2026-09-27T01:05:17.4264997+08:00 — 2026-09-27T01:05:23.3459070+08:00 | 0 | [gate-clippy.log](C-R6证据/gate-clippy.log) |
| `build.ps1 -Action wasm-check` | 2026-09-27T01:05:23.3470320+08:00 — 2026-09-27T01:06:02.3597707+08:00 | 0 | [gate-wasm-check.log](C-R6证据/gate-wasm-check.log) |

原始字节与发布字节SHA-256、路径/换行转换见[证据manifest](C-R6证据/manifest.json)。发布副本采用LF且无尾部空行；原始日志原地保留、不覆盖。cli-stage-fmt与gate-fmt无输出，Tee-Object未建原始文件；仅命令账记录exit0，不补造日志/哈希。早期check辅助日志不全有精确开始时间，不推算时间。
