# 员工 C：新增 Turn HTTP 接入 第 2.0 轮报告

日期：2026-09-23。任务：C-R2-01 / NEXT-01 修订1；执行标识 `C-R2-01-20260923-0701`。范围以 [任务契约](../../任务/第二轮/新增Turn-HTTP契约.md) 与 [联合验收矩阵](../../任务/第二轮/联合验收矩阵.md) 为准。本报告不宣布审查通过；提交不等于 Git 提交、验收、合并或发布。

## 1. 本轮范围与基线

写入范围（契约指定的唯一源码写入范围）：

- `NoManCode/rust-app/src/server.rs`：挂载 `POST /api/sessions/{id}/turns`，新增 `ChatTurnBody` DTO、`required_idempotency_key`、`turn_command_error` 专用映射、`post_session_turn` handler，以及 4 个 server 模块内单测。
- `NoManCode/rust-app/tests/turn_http.rs`：新增 2529 行联合验收测试 n01–n17。
- `niuma/员工C/提交报告/第二轮/`：本报告与 `NEXT-01证据/` 附件。

未修改：旧 `http_contract`/`session_turns` 测试、Engine、Store、Repository、domain、secrets、protocol、schema、Cargo/锁文件、构建脚本、UI。

基线核对（依据 [2026-09-23NEXT-01工作区基线.json](../../../项目经理/审查记录/2026-09-23NEXT-01工作区基线.json)，86 文件清单）：HEAD `8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`、分支 main、共享工作树 `./`。`server.rs` 基线 29547 字节 / sha256 `65031F68…`（清单第 402 行条目）；`turn_http.rs` 不在清单（本轮新文件）。`git status --short` 两文件均 `??`（未跟踪，与基线"shared-working-tree-candidate"一致）。旧路径 `D rust-app/src/server.rs` 属未提交迁移事实，未做 reset/clean。

## 2. 实现入口

| 入口 | 位置 | 说明 |
| --- | --- | --- |
| 路由 | `NoManCode/rust-app/src/server.rs:296-299` | `/api/sessions/{id}/turns` 上 `get(session_turns).post(post_session_turn)`，沿用既有 `DefaultBodyLimit` 1 MiB 与 `guard` 中间件 |
| DTO | `NoManCode/rust-app/src/server.rs:554-560` | `ChatTurnBody` 严格三字段（agent_id / expected_last_turn_id / message），`deny_unknown_fields`；Session 只取 path |
| key 校验 | `NoManCode/rust-app/src/server.rs:575-593` | `required_idempotency_key`：必填、唯一（拒重复头）、拒逗号合并值、可见 ASCII、复用 `secrets::validate_idempotency_key`，不允许 body/query 补头 |
| handler | `NoManCode/rust-app/src/server.rs:642-687` | `post_session_turn`：ID/消息格式校验 → key → 仅调 `Engine::send_chat_turn`；新建 201、回放 200；body `{turn, task, replayed}`。HTTP 层不查 latest、不拼 SQL、不生成业务 ID、不实现幂等 |
| 错误映射 | `NoManCode/rust-app/src/server.rs:602-640` | `turn_command_error`：`IdempotencyConflict`→409；`ChatTurnError` InvalidInput/UnsupportedSession→400、NotFound→404、StalePredecessor/SessionBusy/PredecessorChanged→409、CorruptState→500；transient store→500 retryable=true；其余→安全 500 固定文案。不经默认 ApiError、不套 read_error、不按文案判断 |
| 映射单测 | `NoManCode/rust-app/src/server.rs:1060-1195` | `turn_command_error_maps_every_stable_variant`（穷尽 7 变体）、`turn_command_error_keeps_typed_classification_inside_a_chain`（context 链穿透）、`turn_command_error_never_turns_unknown_failures_into_4xx`（缺行/busy/locked/普通 anyhow→500）、`required_idempotency_key_accepts_exactly_one_clean_value` |

## 3. 本轮修正的实现缺陷

初版 `turn_command_error` 用 `error.chain().find_map(|c| c.is::<ChatTurnError>())` 分类。anyhow 的 `chain()` 元素对 `context()` 包装层 `is::<T>` 恒为 false：n13 实测 `sessions/turns.created_at=-1` 时，Store `classify_row` 产出的 `.context(CorruptState)` 链无法识别，落到默认 500 臂（状态/code/retryable 碰巧相同，但分支和消息错误，且不会返回 typed 消息）。已改为 `error.is::<IdempotencyConflict>()` + `error.downcast_ref::<ChatTurnError>()`（anyhow 原生 downcast 穿透 context 层），由 `turn_command_error_keeps_typed_classification_inside_a_chain` 固定。连带修正 `busy_corrupt` 单测预期：SQLITE_BUSY 外裹 CorruptState context 时 CorruptState 臂胜出 → 500/internal/retryable=false + typed 消息。

## 4. 联合验收矩阵逐项

夹具：每项真实 loopback HTTP server + 临时 SQLite + 本机 mock Provider（`hold:` 前缀消息发一个持久 delta 后由 `Notify` 挂起，不靠 sleep）；首轮经真实 `POST /api/runs` 建成并等待 completed；11 表全内容快照（rowid 序全单元格）对比，行数与旧行改写均可证；Provider 断言取 `calls_for` 消息级差值。

| 编号 | 测试名（`tests/turn_http.rs`） | 观察结果 | 状态 |
| --- | --- | --- | --- |
| N01 | `n01_append_and_chain` (:663) | 首轮完成后 POST 201/replayed=false，再追加第二轮；每轮独立准确 Turn/Task；projects/sessions/agents/runs 行数不增，turns/turn_tasks/tasks 各 +1 且旧行前缀不变；`idempotency` 与 `idempotency_records` 同 key 同命令；每条新消息 `calls_for` +1 | 通过 |
| N02 | `n02_session_sse_stream` (:765) | 从首轮游标订阅 SSE，POST 新轮；回执、GET turns、GET turn、SSE 的 session_id/turn_id/task_id 一致；SSE id=seq、重连仅更大 seq；未泄漏 Run 首任务 | 通过 |
| N03 | `n03_replay_variants` (:863) | 同 key 运行中重试、完成后重试、字段换序/等价转义、更后轮后回放旧请求：均 200/replayed=true、原 Turn/Task 身份不变；消息只执行一次、无新增对象/幂等项 | 通过 |
| N04 | `n04_key_binding_conflict` (:942) | 同 key 分别改 message/agent/session/前序 + 复用首轮 Run key：均 409/conflict/false 固定安全文案；11 表快照不变、`calls_for`=0；身份变更用已有合法对象证明冲突先于资格检查 | 通过 |
| N05 | `n05_body_and_id_validation` (:1027) | 缺字段/null/非字符串/重复字段/未知字段/body session_id与key/无效 path UTF-8/空白·控制·超128字节 ID/敏感标识：均 400/request_failed/false，error=message，无回显、无副作用；非 UUID 合法 ID 可追加 | 通过 |
| N06 | `n06_idempotency_header` (:1191) | 缺头/重复同值/重复异值/逗号合并/空白/201字节/敏感值 → 400；200 字节普通 key → 201 且 200 回放；header 名大小写等价；非 ASCII（`kéy`、`0x80`）在 `HeaderValue::from_bytes` 构造即拒（传输层证据）；不 trim/解码 | 通过 |
| N07 | `n07_message_bounds_and_body_limit` (:1332) | 空白消息 400；100000/100001 UTF-8 字节与多字节边界正确分侧；累计 JSON>1500000 → 400；无/错 Content-Type → 415；body>1MiB → 413；边界正例可追加；原始 message 原样进入 mock（不擅自 trim） | 通过 |
| N08 | `n08_write_guard` (:1467) | 错 Host/错 Origin/cross-site/缺·错写 token（body 本来合法）：403/forbidden/false + 三项安全头、无新行/模型调用；合法写也带安全头 | 通过 |
| N09 | `n09_session_shape_and_404` (:1530) | 不存在 Session/Agent/前序 → typed 404；跨 Session Agent/Turn、Team/多 Agent、带工具权限/工具历史 → 400；非数组 tool_calls 与 role=tool 拒绝、空数组允许；每例无副作用且非无关 FK/路由失败 | 通过 |
| N10 | `n10_predecessor_states` (:1719) | 有后继后用新 key 提交旧前序、顶着 running Turn、cancelled/interrupted 终态：均 409/conflict/false，注入后快照验证无写入；`hold:` mock 阻停保证真忙态 | 通过 |
| N10-映射 | server 单测 `turn_command_error_maps_every_stable_variant` + 继承证据 | `PredecessorChanged` 无法经 HTTP 稳定构造（候选对象与提交在同一锁内），映射由穷尽单测覆盖；引擎侧正式回归用 B 既有 `t10_historical_resume_loses_to_a_newer_turn`（`tests/session_turns.rs:1187`，snapshot-race 断言 `ChatTurnError::PredecessorChanged`），本轮未给它加 HTTP 钩子 | 通过（继承证据，注明非本轮新跑） |
| N11 | `n11_same_key_concurrency` (:1791) | 同 key 双发：恰一个 201 一个 200、同 Turn/Task、`calls_for`=1、五表各只增一组、queued 事件恰一条；再用第二个 `Engine::new(Store::open(同库))` + `Barrier` 双 HTTP 客户端复测，结果一致——排除单 Engine gate 串行掩盖 | 通过 |
| N12 | `n12_different_key_race` (:1930) | 异 key 同前序屏障并发：恰一个 201、另一个 409 typed 冲突；仅胜者增对象/幂等/调用；失败 key 读取最新已完成前序后可作为新命令成功 | 通过 |
| N13 | `n13_storage_faults_safe_500` (:2036) | 有效静止库分别注入 sessions/agents/turns `created_at=-1`（u64 OutOfRange→classify_row→CorruptState context；agents 经 i64<0 直接 typed）、删除既有 Turn 全部 events、`tasks.value` 非法 JSON、`deny_turn_events` 触发器：均 500/internal/false，非 400/404；注入后快照 11 表不变、Provider 0 调用；触发器中止整笔回滚（idempotency_records 无该行）；响应不含 `denied`/db 路径/`SENTINEL-XYZ` | 通过 |
| N14 | `n14_config_faults_500_and_replay_first` (:2142) | 清空 routes、删 `route::chat-route` 凭据、改 base_url 到不可达：新命令均 500/internal/false 无副作用；route 删除后同 key 仍 200 回放同 turn（回放先于路由检查）；恢复配置后新命令 201。按契约本轮配置错误无 typed 分类，统一安全 500 | 通过 |
| N15 | `n15_disconnect_mid_run` (:2185) | SSE 真读到 `partial-disconnect` delta 后断开：后台仍 busy、释放 mock 后 completed、`calls_for`=1；同 key 200 回放；重连 `after=consumed` 仅更大 seq、归属正确 | 通过 |
| N16 | `n16_real_restart_partial_replay_resume` (:2264) | partial 落库后停掉拥有任务的 Runtime（TempDir 跨 Runtime 持有），同库 `Store::open`+`recover`+新 Engine+HTTP：Task/Turn interrupted、partial 保留；同 key POST 200 回放同身份、Provider 不增、未自动 busy | 通过 |
| N17 | `n17_regression_surface` (:2485) + 全量套件 | 原 GET/Run/Task/SSE 成功与错误路径保持（见 §5 全量结果）；映射单测穷尽 ChatTurnError 7 变体、IdempotencyConflict、context 链、busy/locked、原始缺行/普通 anyhow | 通过 |

## 5. 实际执行命令与结果

环境：真实 pwsh（Git 自带 shim，PowerShell 5.1 语义，`-NoProfile -ExecutionPolicy Bypass`）；`CARGO_BUILD_JOBS=1`（本机 32 位 link.exe 并行链接 LNK1102 OOM 的既定规避）；MSVC 环境经 `VSINSTALLDIR`/`WindowsSdkDir` 由 build.ps1 自举。

| 命令 | 结果 | 退出码 |
| --- | --- | --- |
| `pwsh -NoProfile -File ./build.ps1 -Action test` | lib 53、main 0、assessment_adversarial 4、final_acceptance 9、http_contract 11、live 1 忽略、plugin_catalog 17、runtime 10、session_turns 17、**turn_http 17**、peachsh_protocol 5、peachsh_ui 0、doc-tests 0；共 143 通过 0 失败 1 忽略 | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action fmt` | 通过（检查模式，未全仓格式化） | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action clippy` | 通过；仅既有依赖 `proc-macro-error2` future-incompat 提示 | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action wasm-check` | 通过；同上提示 | 0 |
| `git diff --check`（仓库根） | 通过；`README.md` CRLF 为 warning 非差异 | 0 |

日志：`NEXT-01证据/test-workspace.txt`（全量 test 完整输出）、`NEXT-01证据/build-actions.txt`（fmt/clippy/wasm-check）、`NEXT-01证据/哈希与git状态.txt`。付费 live 保持 ignored（需 PEACHSH_TEST_KEY），未执行。

## 6. 本轮环境修补（系统级，可逆，已核实非源码改动）

- 本机 MSVC `VC\Tools\MSVC\14.43.34808\lib\x64` 缺标准导入库（非标准安装），`peachsh_ui.dll` 链接 LNK1104。已从同版本 `lib\onecore\x64` 复制 20 个缺失 `.lib`（msvcrt、vcruntime、msvcrtd、msvcprt(d)、oldnames、legacy_stdio_*、vcomp(d)、vccorlib(d)、pgort/pgocr(k)/pgosweepsysapi、diaguids、msdia140、iso_stdio_wide_specifiers、exe_initialize_mta）到 `lib\x64`。
- `runtime` 测试 `run_command` 工具需 `pwsh.exe`：本机 PATH 上没有 PowerShell 7，仅 Git 内嵌的 pwsh shim（PowerShell 5.1 语义）；已把该 shim 所在目录追加进 PATH 后复测，原失败项 `replace_existing_file_preserves_hardlink_and_command_permission` 通过。具体绝对路径保留在当次原始日志，不作可移植入口。
- 上述均为机器环境补齐，未改任何 crate/测试代码以迁就本机。

## 7. 最终源码哈希与差异

| 文件 | 行数 | sha256 |
| --- | --- | --- |
| `NoManCode/rust-app/src/server.rs` | 1195 | `d89f035c0fdb8ca8b3e316e73e2d8f80a3e134d535e6a66e743d20f1f9959667` |
| `NoManCode/rust-app/tests/turn_http.rs` | 2529 | `430a6f9e8792d95f0e9d95b02736d4a71a7c6242a543024167f4737638f629c8` |

`git status --short`：两文件 `??`（未跟踪；新文件 diff 不含之，格式以 `cargo fmt --check` 全量 0 退出码覆盖）。

## 8. 失败 / 未执行 / 历史结果区分

- 本轮失败：无（`-Action test` 全绿）。
- 未执行：live（付费、需凭据，按矩阵保持 ignored）。
- 历史结果不再代表当前：`runtime::replace_existing_file_preserves_hardlink_and_command_permission` 曾在无 pwsh PATH 时失败（program not found），PATH 补齐后本轮通过；第 1.2 轮报告中的 `t9` 失败与 `d07` 在途失败本轮均通过。
- 继承证据：N10 `PredecessorChanged` 引擎侧用 B `t10_historical_resume_loses_to_a_newer_turn`（本轮复跑通过）；其余全为本轮新跑。

## 9. 边界声明

- HTTP 层只校验格式并调 `Engine::send_chat_turn`；回放/资格/路由顺序、幂等存储、事务回滚均在 Engine/Store，本轮未改。
- 配置/路由普通 anyhow 错误按既定决定统一安全 500（无 typed 分类），报告如实标注。
- 本报告仅为提交物，不等于 Git 提交、经理验收、合并或发布。
