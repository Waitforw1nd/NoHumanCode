> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 第 1.2 轮报告：Engine 连续 Chat 回合返工

日期：2026-09-23  
角色：员工 B。本报告只记录返工和实测证据，不宣布审查通过。  
第 1 轮和第 1.1 轮报告保留。

## 1. 基线

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`  
未提交，未发布，未替换运行实例。

B-R1、B-R2、B-R4 的已闭合实现保留，没有重写事务。没有修改 `server.rs`、`tests/http_contract.rs`、schema 或 protocol。

本轮写入：

- `rust-app/src/domain.rs`：`tool_free_chat_prefix`
- `rust-app/src/engine.rs`：改用上述前缀规则
- `rust-app/src/store.rs`：追加事务也拒绝带工具前缀
- `rust-app/src/repository.rs`：Session/Turn 查找不再用中文文案分类
- `rust-app/tests/session_turns.rs`：补齐能命中目标分支的夹具

## 2. B-R1～B-R5

| 编号 | 本轮状态 | 说明 |
| --- | --- | --- |
| B-R1 | 保持闭合 | 仍按每个 Turn 的 `MIN(seq)` 判断；T6 现在用反向 ID 证明不是字典序碰巧相同 |
| B-R2 | 保持闭合 | 写事务内快照比较未改；T10 仍覆盖 resume 完成后的旧候选 |
| B-R3 | 本轮按无工具范围收紧 | 见下 |
| B-R4 | 保持闭合 | 另补了不叠第二 Agent 的 workspace 夹具 |
| B-R5 | 本轮校正夹具 | 见下 |
| B-C1 | 本轮补齐 Session/Turn | 见下 |

### B-R3

入口：`domain::tool_free_chat_prefix`，Engine 预检和 `Store::append_chat_turn` 都调用。

本切片不支持工具历史。规则：

- 必须有非空文本前缀，且至少一条 user。
- `tool_calls` 缺失，或显式空数组，视为没有工具。
- `role=tool`、非空 tool_calls、非数组 tool_calls 一律 `UnsupportedSession`。
- 不删除字段，不裁剪，不补伪结果。

`t7_each_prefix_fault_is_the_only_reason` 从已完成的正常前缀出发，每次只换一种故障：空历史、assistant content 为 null、object 型 tool_calls、已配对但 `function:{}`、结构完整且已配对的工具历史。五项都是 `UnsupportedSession`，Turn 数不增加，Provider 调用不增加，`!is_busy()`。纯文本连续三轮仍由 T1 通过。

### B-R5

| 夹具 | 现在实际打到的分支 |
| --- | --- |
| T9 部分流 | mock 先发 `partial-b-r5` 的 SSE delta，再等待。独立线程上的 Runtime 在该 Task 的 delta 落库后被 drop。重开后该 Task 为 interrupted，output 含 `partial-b-r5`，模型仍是 `chat-model`。同 key 回放不增加调用，也不 busy。已取消重开、只提交未 launch 仍是另外两个夹具 |
| T3 | 两个 Engine、各自的锁、同一个数据库，Barrier 后同时 `send_chat_turn`。一个 `replayed=false`，Turn ID 相同，该消息在 mock 中只出现一次 |
| T5 / T3 Store | 两个 Store 连接用线程 Barrier 同时进入追加。不同 key 只有一个成功；同 key 一个创建、一个回放 |
| T6 | 旧 Turn 固定为 `zzzz-old`，新 Turn 固定为 `aaaa-new`，同秒。断言 `zzzz-old > aaaa-new` 且旧首事件 seq 更小。latest 是 `aaaa-new`，旧 ID 追加是 `StalePredecessor`。补旧事件后 latest 不变，旧 resume 被拒绝 |
| T11 route | 新的合法短前缀上只删除 route。错误文本含“原模型路由已移除”，不是 `ChatTurnError`，调用和 Turn 数不增加。超长累计上下文仍在另一个夹具，用短新消息命中 `InvalidInput` |
| workspace | 单独的单 Agent 夹具只改前序 workspace。Engine 和直接 Store 追加都是 `UnsupportedSession`，候选路径与错误前序一致，不会先被快照差异拒绝 |
| resume 运行中 | 真实 resume 进入 running 且当前 Task 已有 `resume-ok` delta 后追加，得到 `SessionBusy`，Turn 数不增加。释放 mock 后该 resume 完成 |

没有把“计划覆盖”写成全部 T1～T12 的每一句旧描述都已验证。上表是本轮实际命中的分支。

### B-C1

`session_lookup` / `turn_lookup` 使用 `session_row` / `turn_row`。`QueryReturnedNoRows` 才是 `NotFound`。`FromSqlConversionFailure` 和 `IntegralValueOutOfRange` 返回 `CorruptState`，并用 `context` 保留底层原因。其他存储错误继续向外传播。

`session_and_turn_decode_failures_are_corrupt_not_missing`：把已有 Session 或 Turn 的 `created_at` 设为 -1，两者都是 `CorruptState`，不是 `NotFound`。Agent 负时间仍走上一轮的 `CorruptState`。没有改 HTTP 映射。

## 3. 检查

宿主是 PowerShell 7.6.5：

`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`

测试进程 PATH 另外包含既有 `peachsh-pwsh-shim`，只为旧命令测试能启动 `pwsh.exe`。该 shim 不是本次 PS7 验证。

| 检查 | 结果 |
| --- | --- |
| `build.ps1 -Action test` | 99 通过，0 失败，1 忽略；退出码 0 |
| `build.ps1 -Action fmt` | 退出码 0 |
| `build.ps1 -Action clippy` | 退出码 0 |
| `build.ps1 -Action wasm-check` | 退出码 0 |
| `git diff --check` | 退出码 0 |

99 项含 C 当前的 11 个 `http_contract` 测试。它们这次通过，但不代表 C 已验收，也不是本轮修改。B 的 `session_turns` 为 17 通过。付费 live 测试未执行。

## 4. 未完成

1. 新命令仍未接 HTTP。`CorruptState` 不应映射为 404，映射仍等负责人确认。
2. `MIN(seq)` 仍不能识别“只删创建事件、后续事件还在”。
3. 空 `tool_calls` 数组按“没有工具”接受；非空工具历史明确不支持。没有实现完整工具消息协议。
4. 未提交，未改 C 的文件。
