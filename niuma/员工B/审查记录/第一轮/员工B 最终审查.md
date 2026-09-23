> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 B 最终审查：通过

审查日期：2026-09-23

范围：已有单 Agent、无工具 Chat Session 的后续 Turn 应用服务，以及必要的 Store 原子追加、历史保护、幂等和恢复兼容。

结论：**APPROVED（本轮范围）。B-R1～B-R5 全部关闭，B-C1 原诊断反例关闭。**

实现与正式测试由员工 B 完成。负责人依据第 1.2 轮交付做只读审查、隔离复验并确认放行；本轮没有接管共享生产代码或正式测试。历轮报告与 review 保留，本文件作为 B 第一轮最终交接入口。

详情：[review 1.2](<员工B review 1.2.md>)。证据：[审查附件](员工B第1.2轮审查附件/README.md)。

## 1. 已验收链路

```text
Engine::send_chat_turn
→ 校验消息/key，先判断同 key 回放
→ 验证 Session / Agent / 前序归属、无工具历史、路由与上下文大小
→ Store::append_chat_turn 的立即事务
→ 再查回放、最新已完成前序、单 Agent、workspace 与真实前序快照
→ 同事务写 Turn / TurnTask / legacy Task / 幂等 / 初始事件
→ 提交成功且非回放后 launch
```

连续消息复用 Project/Session/Agent/legacy Run，新建独立 Turn/Task，保留旧轮结果。提交顺序以每轮首个现存事件 seq 判定，旧轮补事件不改变 latest。合法 resume 改变前序内容后，旧候选不能漏历史提交；已有后继的历史 Chat Task 不能 resume 重开。

真实部分输出重启会保留当前 Task 草稿并标 interrupted；已持久 cancelled 保持 cancelled；已提交未 launch 也按中断恢复处理。重复 send 只回放，显式 resume 才继续执行。

## 2. 可供下一阶段复用的契约

入口：[Engine::send_chat_turn](../../../../NoManCode/rust-app/src/engine.rs#L327)，返回 `anyhow::Result<ChatTurnReceipt>`；调用方按类型识别业务错误，不能解析中文显示文本。

| 对象 | 本轮字段/含义 |
| --- | --- |
| SendChatTurn | session_id、agent_id、expected_last_turn_id、message、idempotency_key |
| ChatTurnReceipt | turn、task、replayed；返回准确的该轮对象，不能从 legacy Run 的第一个 Task 猜测 |
| IdempotencyConflict | 同 key 不同稳定请求，或与不适用的 legacy Run key 冲突 |
| ChatTurnError | InvalidInput、NotFound、UnsupportedSession、StalePredecessor、SessionBusy、PredecessorChanged、CorruptState |

契约要点：

- 同 key 回放先于最新前序/可追加资格检查；即使已有更后轮次，仍可回放原 Turn/Task，不再次 launch。
- 前缀只支持文本 system/user/assistant 且至少有 user；tool_calls 缺失或空数组允许，role=tool、非空或非法类型的 tool_calls 明确拒绝。不要裁剪或伪造历史来适配。
- 新消息不得空白，字节数不超过 100,000；拼接后的 messages JSON 不超过 1,500,000 字节，超限失败，不自动压缩。
- 既有项目、workspace 和路由快照保持；实际凭据继续通过既有受保护入口获取，配置 route 被移除或地址不匹配时失败。
- CorruptState 与未分类存储故障不能当作不存在。新 HTTP 接入须显式映射已知业务错误，并为剩余存储错误提供安全 500；最终 HTTP DTO/状态码矩阵在接入任务中确认。

定义见 [domain.rs](../../../../NoManCode/rust-app/src/domain.rs#L489)，原子追加见 [store.rs](../../../../NoManCode/rust-app/src/store.rs#L590)。本轮没有更改 schema/protocol。

## 3. 实际验收结果

固定源码全量 **99 项通过、0 失败、1 项付费 live 忽略**，其中 session_turns 为 17 项；fmt、Clippy、WASM 均退出 0。额外 8 个历史反例及 2 个边界探针全部通过，未计入正式 99。

B-R3 原反例现在没有新增 Turn 或 Provider 调用。直接 Store 及 Engine 的工具历史拒绝已补验 11 张表计数保持，空工具数组正例通过。真实分块输出、Runtime 停止、并发屏障、反向 ID、独立故障和运行中 resume 的正式回归均成立。

基线是 `8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5` 加有效未提交工作区修改；附件保存 B 四个实现文件、正式测试、关键 SHA256 与实际日志。使用真实 PowerShell 7.6.5、临时库和本机 mock，没有付费调用、提交或发布。

## 4. 放行后的方向与限制

B 和 [C 的现有 HTTP/SSE 范围](<../../../员工C/审查记录/第一轮/员工C 最终审查.md>) 现在均已通过。下一阶段可接新增 Turn HTTP 写入口并做跨链路验收；该接口此刻尚不存在。本次不把 CLI/UI、工具消息、Team、持久 Scheduler 或全产品发布列为完成。

MIN(seq) 不检测仅删除创建事件但保留后续事件的损坏，也不提供分布式恰好一次执行。保留这些已确认边界。

非阻断后续改进：将取消/未 launch 重开夹具的 TempDir 也外置；正式测试可吸收更完整的副作用计数、空数组正例、Engine 并发 Task/事件断言；整理错位注释。具体入口和原因见 review 1.2，不需要为这些重新开启 B 第 1.3 轮。
