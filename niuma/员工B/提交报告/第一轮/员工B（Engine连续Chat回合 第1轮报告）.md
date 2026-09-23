> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 第 1 轮报告：Engine 连续 Chat 回合

日期：2026-09-23  
角色：员工 B，Engine 应用服务实现。  
本报告只记录实现和实测证据，不宣布审查通过。

## 1. 任务与代码基线

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`  
分支：`main`  
未创建提交，未发布，未替换运行中的实例。

开工时工作区已有员工 A 最终审查通过但未提交的修复。这些文件保持原样，没有 reset、checkout 或覆盖：

- `PROJECT-BOOK.md`
- `records/CONVERSATION-FULL.md`
- `records/PROJECT-PROGRESS-SUMMARY.md`
- `rust-app/crates/protocol/src/lib.rs`
- `rust-app/src/secrets.rs`
- `rust-app/src/server.rs`
- `rust-app/tests/runtime.rs`
- `rust-app/tests/final_acceptance.rs`（未跟踪，A 的验收测试）

本轮写入：

| 文件 | 本轮内容 |
| --- | --- |
| `rust-app/src/domain.rs` | `SendChatTurn`、`ChatTurnReceipt`、`ChatTurnError`、稳定摘要；纠正 `legacy_run_id` 恒等旧注释 |
| `rust-app/src/engine.rs` | `Engine::send_chat_turn`，提交成功后才 launch |
| `rust-app/src/store.rs` | `append_chat_turn` 立即写事务；Chat 历史轮次 resume 保护 |
| `rust-app/src/repository.rs` | `latest_committed_turn`，按首事件 `seq` 判断提交顺序 |
| `rust-app/tests/session_turns.rs` | T1～T12 集成测试，新增文件 |

`domain.rs`、`engine.rs`、`store.rs`、`repository.rs` 的工作区 diff 同时包含 A 已验收的未提交修复。本轮没有重做创建、统一幂等、状态事务或 Session SSE。

## 2. B-01～B-07 实现入口

| 条目 | 入口 | 状态 |
| --- | --- | --- |
| B-01 新消息创建新回合 | `Engine::send_chat_turn` | 已实现且验证 |
| B-02 首轮限制 | Engine 预检 + `Store::append_chat_turn` 事务内复查 | 已实现且验证 |
| B-03 保持模型、项目、上下文 | 复制上一轮 Task 的 route、消息前缀和 Project.root_path | 已实现且验证 |
| B-04 先回放再验证资格 | `idempotency_replay` / 事务内 `classify_both` | 已实现且验证 |
| B-05 持久顺序判断最新回合 | `Repository::latest_committed_turn` | 已实现且验证 |
| B-06 原子追加，成功后才派发 | `Store::append_chat_turn` 后 `Engine::launch` | 已实现且验证 |
| B-07 保护历史、恢复和旧入口 | `ensure_resume_targets_latest_chat_turn`；旧 start/resume/recover 保留 | 已实现且验证 |

未实现、按任务明确排除：HTTP 写路由、聊天 UI、分支/编辑历史、换模型、自动摘要、工具审批、Team/Plan 追加、持久 Scheduler、恰好一次执行。

## 3. 供员工 C 使用的应用命令

```rust
pub async fn Engine::send_chat_turn(
    self: &Arc<Self>,
    command: SendChatTurn,
) -> Result<ChatTurnReceipt>
```

```rust
pub struct SendChatTurn {
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub expected_last_turn_id: TurnId,
    pub message: String,
    pub idempotency_key: String,
}

pub struct ChatTurnReceipt {
    pub turn: Turn,
    pub task: TurnTask,
    pub replayed: bool,
}
```

成功新建：`replayed=false`，`receipt.turn.id` 和 `receipt.task.id` 是本轮新对象。不要用整个 `Run` 或 `run.tasks[0]` 表示本轮。

同 key、同摘要回放：`replayed=true`，返回原 Turn/Task，不调用 Provider，不新增事件。即使该轮已经不是最新轮次，也回放这一轮。

摘要只包含命令版本 `engine.send_chat_turn.v1`、SessionId、AgentId、expected_last_turn_id 和原始 message。不包含新 ID、时间、全局设置或运行状态。

错误分类：

| 条件 | 调用方可 downcast 的类型 |
| --- | --- |
| 同 key，但消息、Session、Agent 或前序不同 | `store::IdempotencyConflict` |
| key 已绑定 legacy-only Run 或其他命令 | `store::IdempotencyConflict` |
| 前序不是已提交的最新回合 | `ChatTurnError::StalePredecessor` |
| 最新回合或任务不是 completed | `ChatTurnError::SessionBusy` |
| 非 Chat、多任务、带工具、归属错误、非法上下文 | `ChatTurnError::UnsupportedSession` |
| 空消息、超长消息、非法 key、上下文超过 1,500,000 字节 | `ChatTurnError::InvalidInput` |
| Session、Agent、Turn 不存在 | `ChatTurnError::NotFound` |
| 回合缺少首事件，无法判断顺序 | 明确错误，文本含“首条事件”；不猜测 |

中文 Display 只给人看。C 应按类型分支，不要解析中文。本轮没有新增 HTTP 错误码映射；若 C 暴露该命令，`IdempotencyConflict` 可继续走现有 `409/conflict`，`ChatTurnError` 的 HTTP 映射需要负责人确认。

## 4. 事务顺序、最新回合和重启边界

`append_chat_turn` 使用 `BEGIN IMMEDIATE`，顺序是：

1. `classify_both` 分类 key。匹配则返回原 Turn/Task，`replayed=true`。
2. 检查 Session 是 Chat、Project 一致、Agent 属于该 Session。
3. 用首事件最大 `seq` 读取真实最新回合，核对请求前序。
4. 最新 Turn 和唯一 Task 都必须是 completed、无依赖、无工具。
5. 复用 `commit_turn_in_tx` 插入新 Turn、新旧 Task、幂等记录和初始事件。
6. 提交后 Engine 才 `launch`。回放路径不 launch。

`turns_for_session` 仍按 `created_at,id` 排序，本轮没有改它。追加和 resume 保护使用 `latest_committed_turn`。以后如果列表要显示真实先后，不能直接复用当前列表排序；秒精度时间和 UUID 字典序都不代表提交顺序。

重启沿用 `Store::recover`：已落库的 cancelled 保持 cancelled；重启时仍为 queued/running 的任务转为 interrupted，并保留草稿和 route/model。重启与同 key 回放不调用模型。用户对最新轮次显式 `resume` 才重新执行。历史 Chat 轮次在 resume 事务内拒绝。legacy-only 和 Team 的 resume 语义没有改。

数据库提交后、launch 前崩溃，重启会看到 queued 并标为 interrupted。本轮不承诺分布式恰好一次，也没有持久 Scheduler。

## 5. T1～T12 证据

测试文件：`rust-app/tests/session_turns.rs`。临时 SQLite，本机 mock Provider，凭据为合成值 `synthetic-chat-key`。

| 编号 | 证据 | 状态 |
| --- | --- | --- |
| T1 | 连续 3 轮后 Project/Session/Agent/Run 仍各 1；Turn/Task 从 1 增到 3。第一轮 output/usage 不变，后续 output 为 `reply`、usage.total_tokens 为 4 | 已验证 |
| T2 | 第 3 次 Provider 消息的 user 内容依次是“第一轮/第二轮/第三轮”，当前 user 一次；无 tool_calls，无 tools | 已验证 |
| T3 | 同 key 顺序回放对象、事件、调用次数不变。后续已有另一轮时仍回放旧轮。两个 Store 连接同 key 并发，恰好一个创建、一个 replay，Turn 数只加 1 | 已验证 |
| T4 | 同 key 改消息或前序返回 `IdempotencyConflict`。legacy-only key 同样冲突。失败后调用次数和对象数不变 | 已验证 |
| T5 | 两个独立 `Store::open` 连接、不同 key、同一前序并发，成功数是 1，失败是 `StalePredecessor`，Turn 数是 2 | 已验证 |
| T6 | 同秒把真实回合改成字典序更后的 ID 后，陈旧 ID 被拒绝；真实最新 ID 可以追加 | 已验证 |
| T7 | 空白消息 `InvalidInput`；错误 Agent/缺失前序 `NotFound`；工具历史、Team、未配对 tool_call 为 `UnsupportedSession`；running 为 `SessionBusy`；缺首事件明确失败。失败无新增对象和调用 | 已验证 |
| T8 | events INSERT trigger 失败后 Turn/Task/幂等记录和事件数全部回到追加前，Provider 调用不增加 | 已验证 |
| T9 | 取消后保持 cancelled，回放不增加调用。手动恢复 running+partial 后 recover 变 interrupted 并保留草稿和模型。显式 resume 才再次调用并完成 | 已验证 |
| T10 | 历史 Chat 轮次 resume 返回 `StalePredecessor`，completed 快照不变。Team/legacy 现有 runtime 与 final_acceptance 仍通过 | 已验证 |
| T11 | route host 变化时追加失败且调用数不增加。全局 workspace 改到其他目录后，新 Task 仍使用 Project.root_path。超过 1,500,000 字节返回 `InvalidInput`，不调用 Provider | 已验证 |
| T12 | 同一 Session 的两个 Turn 都出现在现有 `Store::events` 结果中，seq 严格递增，after 游标不回退 | 已验证 |

route 删除与 host 变化走同一 `Engine::key` 保护。测试覆盖的是 host 变化；删除 route 会在同一函数于调用前失败，没有单独再造一个删除夹具。

## 6. 公共契约、schema、权限和凭据

- Schema 仍是 6。没有新增列、迁移或事件 envelope 字段。
- Protocol ID、状态和错误码语义未改。本轮 DTO 在 domain，不在 protocol。
- 没有新增 HTTP 写路由，没有改 UI、Provider 协议、secrets、workspace 或 WASM。
- 新回合继续 `tools=false`、`allow_commands=false`、`write_scopes=[]`。没有放宽审批。
- 凭据仍由 `Engine::key` 从 DPAPI/环境读取。地址不匹配时失败，不把新 Key 发到旧地址。测试没有读取或打印真实 Key。
- `Session.legacy_run_id` 注释已改为“存储映射，不是 SessionId 别名”。这是纠正与当前创建行为不符的旧注释，不改变字段。

## 7. 实际检查

执行目录：`./rust-app`。  
本机没有 PowerShell 7。`pwsh` 解析到 Windows PowerShell 5.1；`build.ps1` 在该宿主中可执行，并配置了项目既有 MSVC / Windows SDK。没有把其他 shell 冒充为独立验证。

| 检查 | 命令 | 结果 |
| --- | --- | --- |
| 工作区测试 | `pwsh -NoProfile -File build.ps1 -Action test` | 83 通过，0 失败，1 忽略；退出码 0 |
| 格式 | `pwsh -NoProfile -File build.ps1 -Action fmt` | 通过；退出码 0 |
| Clippy | `pwsh -NoProfile -File build.ps1 -Action clippy` | workspace / all-targets / `-D warnings` 通过；退出码 0 |
| WASM | `pwsh -NoProfile -File build.ps1 -Action wasm-check` | 通过；退出码 0 |
| 补丁空白 | `git diff --check` | 通过；退出码 0 |

83 项组成：43 个库测试、4 个 adversarial、9 个 final_acceptance、10 个 runtime、12 个 session_turns、5 个 protocol。相对 A 的 71 通过，本轮新增 12 个集成测试。

忽略项仍是 `xpeach_native_stream_and_coding_tool`。未设置 `PEACHSH_TEST_KEY`，没有执行付费 live 测试。

工具仍提示 `proc-macro-error2 2.0.1` 的未来 Rust 兼容警告。它不是本次编译、测试或 Clippy 失败。

首次直接调用 `build.ps1 -Action test` 时，Git Bash 默认 PATH 没有 `pwsh.exe`，A 的既有命令测试 `replace_existing_file_preserves_hardlink_and_command_permission` 报“无法启动 PowerShell”。把既有 shim `./.local/temp/peachsh-pwsh-shim\pwsh.exe` 放入 PATH 后，该测试与全套测试通过。这是测试进程环境，不是本轮 resume/追加回归。

## 8. 未完成、风险和需要负责人决定的事项

1. HTTP/CLI 尚未接入 `send_chat_turn`。需要决定 `ChatTurnError` 到现有 `ErrorCode` 的映射后再由 C 实现。建议：`StalePredecessor`/`SessionBusy`/`IdempotencyConflict` 为 conflict，`UnsupportedSession` 为 forbidden，`InvalidInput` 为 request_failed，`NotFound` 为 not_found。
2. 缺少首事件时，当前是明确的 anyhow 错误，不是 `ChatTurnError`。如果 C 需要完全不用文本分类，负责人可批准再加一个损坏顺序变体。本轮没有擅自扩大错误枚举的协议面。
3. 列表接口排序与提交顺序仍不同。后续列表改造要单独验收，不能默认 `turns_for_session().last()` 是最新回合。
4. 本轮不包含上下文压缩。上下文超限直接失败；是否允许用户显式摘要后再继续，属于后续产品决定。
5. 提交后、launch 前崩溃仍只会在重启时标 interrupted，不会自动重放模型。这是任务批准的边界，不是持久调度完成。
