# 2026-11-16 恢复、因果顺序与 Host 关闭修复

作者：本轮员工 B。状态：产品候选已提交并按经理要求冻结，等待整套门禁；本文不是全产品验收结论。

## 基线与范围

- 独立工作树：仓库旁 `../nhc-audit-20261116-recovery`；分支 `codex/audit-20261116/recovery`。
- 基线：`bd31edeb80ab6801af6987ee50e719d14f457935`。本轮开始产品目录干净，没有接收旧 B/C 草稿；不接管旧 C 的 47 文件在途修改。
- 本轮用户从只读审计明确授权恢复开发，经理指定员工 B 为本树实现者和普通提交执行人，经理负责整合、主仓身份同步及串行 Cargo 验证。
- 原写集：`NoManCode/rust-app/src/{engine,store,host_services,main}.rs` 和相关 checkpoint/shutdown 测试。后经经理明确扩写授权，将 `src/repository.rs` 的可信 completion delta 插入边界及必要回归纳入本轮。
- 本轮未修改 secrets/provider/workspace/command_execution/build，也未启动子代理或旧 B/C 树。第三方 references 不属于审计对象。
- 已按本轮上下文读取根和 NoManCode 的 AGENTS、开发守则、Git 协作规范、项目书/进度/任务板相关段及主仓员工 B 身份顶部；旧暂停交接的 unknown 不重新判定。

## 三条原始缺陷、触发与修复

### R1：已接受的延迟 resume 输入不耐久

基线中，存在未闭合工具组时，`Engine::resume` 接受输入并恢复 Task，实际消息暂存 `deferred_resume: Mutex<HashMap<...>>`；`run_task` 在工具对账后取出该内存输入。接受之后重启，或消费前执行失败，都会失去输入上下文。普通已闭合消息直接追加的路径并非此反例。

修复入口为 [store.rs](../../../../NoManCode/rust-app/src/store.rs) 的 `resume_task_with_pending_input`、`pending_resume_inputs_tx`、`consume_resume_inputs`，以及 [engine.rs](../../../../NoManCode/rust-app/src/engine.rs) 的 resume/run_task：

- 用现有 events 表保存版本化的待投递 resume 输入，记录接受顺序；不再使用内存 map 作为投递事实。
- 工具组对账闭合后，在 IMMEDIATE 事务内核对持久消息前缀，追加全部待消费输入并写入消费 ack。消息与 ack 同时提交，失败同时回滚。
- ack 按原 resume event seq 对应，不重复投递；普通 `Store::event` 禁止生成保留的接受/消费证明。
- 恢复仍遵守已有工具 claim/result 对账语义，不自动重放已执行工具。

回归 `audit_resume_pending_survives_reopen_and_consumption_rolls_back` 覆盖重开仍存在、工具闭合后追加、重复消费不重复消息，以及消费写入故障后的回滚。主线程已执行通过。

### R2：同路径文件变更使用 wall clock 排序

基线 workspace_changes 的最新变更、完整变更及 checkpoint source 查询按 `created_at,rowid` 排序。时间回拨时，连续写同一个文件的事实顺序会被重排，后续 before/after 链与实际文件不一致。只在同秒或时钟单调的正常测试中观察不到该问题。

修复入口为 [store.rs](../../../../NoManCode/rust-app/src/store.rs) 的 workspace_changes 查询、`checkpoint_sources`、`verify_checkpoint_sources`，以及 [host_services.rs](../../../../NoManCode/rust-app/src/host_services.rs) 的 `restore_locked`：

- 使用 SQLite 已提交插入顺序 `rowid` 排序，同路径最新变更用倒序 rowid。当前实现没有删除这些变更行，不新增 schema 字段。
- 新 manifest 按因果顺序计算；读取旧 manifest 时先尝试因果摘要，不符再核对旧 `created_at,rowid` 摘要。验证成功不重写旧 manifest 字节。
- 对旧同路径 manifest，若其 before/after 意图与因果恢复不同，读取允许，restore 明确返回 `Conflict { receipt: None }`，文件、Task、manifest 与恢复 receipt 均保持原值。

回归包括 [tests/checkpoint.rs](../../../../NoManCode/rust-app/tests/checkpoint.rs) 的三次真实工具写入加逆序时间戳、跨路径旧 manifest 可读且可恢复，以及 `audit_legacy_same_path_checkpoint_reads_but_refuses_different_restore` 的同路径旧摘要负控。主线程已执行上述 audit 范围通过。

### R3：HTTP 退出不足以证明后台工作收束

基线 Ctrl-C 只调用取消后结束 HTTP；执行 worker 未被完整等待。取消审批落库失败可能中断取消路径，且原 `launch` 在执行结果 unknown 后仅返回终态保存结果，保存成功会掩盖外部副作用仍未知。worktree 的 `Ok(receipt.state == Unknown)` 同样不能当作已收束。

修复入口为 [engine.rs](../../../../NoManCode/rust-app/src/engine.rs) 的 `request_shutdown`、`begin_shutdown`、`shutdown`、`spawn_task_worker`、`spawn_worktree_worker`，及 [main.rs](../../../../NoManCode/rust-app/src/main.rs)：

- 先封闭准入、关闭 slot 并无条件发送 cancel，再做审批取消持久化。保留 cutoff 时任务集合，即使 worker 已从 active 移除也不会漏取消持久化。
- shutdown 的同一个 deadline 覆盖 gate/cutoff 等待及后台等待；排队 start/resume 在拿到 gate 后重新拒绝。`shutdown_persistence_started` 保证重复 shutdown 不重复启动持久化副作用。
- JoinSet 持有 task/worktree/关闭持久化 worker，显式观察完成、错误和 panic；超时不 abort、不丢原句柄，允许后续观察迟到结果。
- 独立保留 `external_outcome_unknown`：识别 error chain 中的 `CommandOutcomeUnknown`、审批 `UnknownResult`、持久审批 `Claimed/Unknown`，以及 worktree `Unknown/Claimed` 回执。普通 Provider 业务失败和明确 Rejected 不因此被标为外部结果未知。
- `ShutdownReport::settled()` 要求没有剩余 worker、没有持久化/worker 失败、没有外部 unknown、没有超时。CLI 收到 unknown 返回错误，非成功退出。
- instance lock 由 `main` 持有并跨越 runtime teardown；teardown 返回原锁。正常收束时可释放；Err/unknown 的最终 main 退出路径有意保留锁句柄至 OS 进程结束，防止 teardown 超时后旧 blocking writer 与新实例恢复并发。该 process-lifetime 持有只在最终退出路径使用，不是在长期运行服务中不断泄漏句柄。

回归经过生产 worker 包装层，验证 typed/untyped unknown 下终态保存成功仍不 settled，并有普通失败/Rejected 负控；另覆盖取消写库失败仍收到 cancel、gate 锁竞争与 start/resume 排队、超时保留 worker/迟到完成。main 的两个有界合成 blocking worker 测试用第二文件句柄验证 drain 期间和 timeout 返回后原锁仍持有，并显式释放、等待原 worker 后验证锁释放。没有用真实子进程替代这些夹具。

## 跨 completion 安全接线

该部分为 A/D/经理交叉审后追加的同链修复，A 负责 redactor 算法、CompletionLease 和 provider_runtime_tests，B 仅修改 Engine/Store/Repository 接线：

- Engine 在整个 run_task 的 rounds 外持有一个 `StreamRedactor`，传给 A 的 `CompletionLease::execute_with_redactor`，在 raw decoder 边界保留同一状态。
- 仅最后 `NoToolCalls` 且策略为 `Finish` 时 finish；失败、取消、轮数耗尽不释放未决尾部。
- emit 收到的已经是 safe 片段，直接累计输出及 delta。最终 tail 同时加入 task.output 和最后 assistant message.content。
- 无工具的最终 assistant 消息等 Finish/tail 后一次追加保存，不修改已经持久化的消息前缀。带工具的消息仍须先保存再执行工具。
- `Store::completion_delta` 与 `Repository::append_completion_delta` 仅 pub(crate)，仅接收可信 Host 流式文本；后者检查 kind=delta 且 data 仅有字符串 text，并保留公共插入器的身份、会话/回合关联、schema 和游标校验。
- 已通过跨片策略的 `foo` + `sk-plain` 不再被逐片再次脱敏成 `foo[redacted]`。公共 `Store::event` 和 `Repository::append_event` 保持默认脱敏。
- 静态追踪确认现代/legacy Store 事件读取、Repository events_after、Host 事件转发、server SSE 序列化没有再次逐片 redact，无须修改 server。

`audit_completion_delta_preserves_context_and_public_event_redaction` 覆盖现代/legacy 重开与 after-cursor 读取、正常词片段保留、公共 Store/Repository 脱敏仍生效，以及可信入口的形状/关联拒绝。A 的新测试负责跨轮 messages、输出、事件、SSE 和普通文本负控。

`8a66dc8e` 的最初双层过滤方案单独不足以闭合跨 completion 字段身份问题；必须包含后续 `32e58985` raw 边界共享状态、A 的接口/安全 message.content 修改及本次 `b06a1b1b` 投影接线，不应单独发布中间候选。

## 兼容、失败与恢复边界

- schema 保持 17，没有占用未合 U 使用的 schema18；没有迁移或新凭据/权限字段。
- 无 delivery_version 的历史 resume 事件不推测为待投递，不自动重放；旧数据可以打开，但无法凭旧日志补证当时是否已消费。
- 旧 checkpoint 摘要使用明确 fallback 只读验证；同路径语义冲突保守拒绝恢复，不改写旧事实。
- 新的 shutdown unknown 标志不清理持久审批/Worktree unknown；重启继续按既有保守恢复语义处理。对保存失败、无法确认原外部结果的情况不宣称成功。
- Engine 的 30 秒等待、runtime teardown 的 1 秒等待与 OS/子进程寿命是不同边界。本轮不承诺进程 30 秒必退；现有 command collector 不接收 Engine cancel token，Shell 等待可达 600+5 秒，未收束时只能如实 unknown。完整 U 的底层寿命治理不在本轮完成范围。
- 可信 delta API 的调用约束为 Host 已经过共享 StreamRedactor；它不是对任意输入的公开免审写入接口。启发式脱敏也不构成识别任意未知秘密的承诺。

## 提交清单

| 本树提交 | 内容/路径 |
| --- | --- |
| `61154d28db5f9e1e53c99e8d4b7a21c20e4ea9e5` | 初始耐久 resume、因果排序、旧 manifest 兼容、关闭跟踪及 main 接线；engine/store/host_services/main、tests/checkpoint |
| `2392e444f1f6bedde2109dea2268a186e033f1b0` | 同路径旧 manifest 可读但恢复 Conflict 负控；engine |
| `8a66dc8ee0dfba38c40b086c472a005f75334aca` | 跨轮状态初始接线；engine；须结合后续纠正 |
| `32e58985dc519a4e25ed300361666349eac58de9` | 改为 raw decoder 边界共享状态；engine；依赖 A 接口 |
| `e1b0f2b858fae85bb43f6f7ae31a55cd82d095ee` | 独立 unknown、生产 worker 包装层回归、main 锁生命周期及合成锁竞争测试；engine/main |
| `e56173280f1f91087cd6364e0beb4e4379001226` | 修正审批回归的不可变 TaskSpec 夹具：工具权限在创建时设置；engine；不改变生产逻辑 |
| `b06a1b1baad6e3660d9bbf72e4cb1e406d621a31` | 可信 completion delta、final message tail、Store/Repository 正负控；engine/store/repository；本树产品冻结点 |

## 验证记录：本人执行与主线程执行分开

本人执行的是静态检查：授权 Rust 文件的 `rustfmt --edition 2024 --config skip_children=true` 及 `rustfmt --check`、`git diff --check` 和逐次提交 diff 审阅，最终均退出 0。少数前期只读命令曾有 PowerShell 花括号/工作目录错误，修正后重读；这些不是产品验证通过证据。

**本人未执行 Cargo、产品、真实进程测试或旧事故诊断。** 本树不含 A 的 secrets/provider 修改，不能把本树单独编译视为已满足组合依赖。

以下均为经理在 integration 树串行执行后反馈，本文没有在本树重跑：

| 经理已反馈的组合/范围 | 实际结果 | 日志位置（相对 integration 树） |
| --- | --- | --- |
| 初始 `61154d28` recovery 候选 | check 退出 0；lib audit_ 5/5；checkpoint audit_ 1/1 | `.local/audit-20261116/recovery-*.log`、`checkpoint-tests.log` |
| `e1b0f2b8` + 经理所述 `26951f04` 组合 | check 退出 0；lib audit_ 9/9；bin peachsh audit_shutdown_tests 2/2 | `.local/audit-20261116/recovery-tests-attempt2.log`、`host-lock-tests.log` |
| `b06a1b1b` + `26951f04` 组合 | check 退出 0；stream_redaction 8/8；audit_completion_delta 1/1 | 本轮经理即时反馈，具体日志由经理统一归档 |

经理已经接收 A 新测试 `f81cec7a`，正在整合执行最终专项并准备整套门禁；本报告落笔时尚未收到该轮结果，不能列为已通过。

供经理复验的精确选择（均在 `NoManCode/rust-app`，本报告列出不等于本人执行）：

```powershell
cargo check --offline --locked
cargo test --offline --locked --lib audit_ -- --test-threads=1
cargo test --offline --locked --bin peachsh audit_shutdown_tests -- --test-threads=1
cargo test --offline --locked --test checkpoint audit_ -- --test-threads=1
cargo test --offline --locked --lib stream_redaction -- --test-threads=1
cargo test --offline --locked --lib audit_completion_delta -- --test-threads=1
```

## 当前剩余与交接

产品冻结在 `b06a1b1b`；除主线程实测失败返工外，本轮不继续修改产品。下一操作由经理整合 A 新测试，执行最终专项和整套门禁，再维护主仓身份/任务板及验收结论。完整 U/W3 和历史事故结论不因本轮局部通过而改变。本报告只记录本轮实现与已收到的验证证据，不替代主线程验收。

## 后续：完整门禁中的 Worktree HTTP 超时取证

经理完整 workspace 门禁在 `tests/worktree_http.rs` 初始第 351 行失败：创建 POST 的 reqwest 6 秒期限到期，尚未获得 receipt；该 suite 为 1 passed / 1 failed，用时 12.37 秒。原日志保留在 integration 的 `.local/audit-20261116/final-attempt1/test.log`，不能把前面专项通过视为完整门禁通过。

经理随后不修改受测产品，执行单 package 定向 suite，两项通过、14.20 秒；其二进制为 `worktree_http-e320a9fa806f17e7.exe`，与 workspace 特性组合的 `worktree_http-1dced705004efa15.exe` 不同。再执行原 workspace 二进制，第一项通过，第二项初始第 690 行的真实创建 POST 触发同一 6 秒超时，suite 14.68 秒。分别记录在 `worktree-http-reproduce.log`、`worktree-http-workspace-binary.log`；这些是经理执行，本人仅只读核对日志及源码。

静态复核尚未发现 JoinSet 死锁证据：spawn 后任务独立调度，admission gate 在 await oneshot 前释放；已通过的 `successful_worktree_worker_reply_preserves_unknown_state` 先 await worker 回执、之后才调用 shutdown，证明不依赖父 future 轮询 JoinSet。完整门禁中的 current-thread 真实创建、丢弃调用者后仍注册等测试也已通过。`git_worktree.rs` 和 `workspace_identity.rs` 本轮未变。

另核得既有 `worktree_diagnostics.rs` 的单项目调用预算为 claim 前 27 次读取、claim 后 58 次读取和 1 次变更；生产单读取期限 10 秒、变更 30 秒、清理 5 秒。6 秒 HTTP 总期限并非这些生产期限的合计，但目前不能只据此认定超时原因或直接调高测试期限。

经理扩写授权仅 `tests/worktree_http.rs`。新增诊断候选 `04ff7b609cdc79570749c6a5ca2711a0e85285a2`：默认路径保持原客户端期限；显式设置 `NHC_WORKTREE_HTTP_TIMING=1` 才以只读 lookup 观察 before_claim/claimed/终态变化、6 秒时状态及同一个原请求最终状态与耗时。60 秒仅为诊断观察上限，迟于 6 秒响应仍明确失败，不重发创建，不删除或放松原业务断言。本人 rustfmt/check、diff check 退出 0，未 Cargo；该诊断尚待经理在原 workspace 特性组合执行。产品实现继续冻结，正式期限修复须以阶段/耗时证据为依据，不能把诊断候选冒称已修复超时。
