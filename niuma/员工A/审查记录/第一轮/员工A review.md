# 员工 A Review — 第 1 轮

审查对象：niuma/员工A/提交报告/第一轮/员工A（Project-Session-Turn领域契约与SQLite Repository 第1轮报告）.md
审查日期：2026-09-22
审查人：项目总监
结论：CHANGES_REQUIRED

## 已确认通过

- ProjectId、SessionId、TurnId、AgentId、TaskId 已与显示名分离。
- SessionKind 继续由协议字段决定，没有恢复通过 chat-session 推断类型。
- schema 5 → 6 的新表、迁移标记、旧 runs/tasks 兼容读取已经落地。
- commit_turn 对新领域记录、旧 run/task 投影、幂等记录和初始状态事件使用同一个事务。
- 同 key 同摘要复用、同 key 不同摘要冲突已有测试。
- 新增 schema 迁移、回滚、恢复、事件游标测试。
- 重新运行了：
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action fmt
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action clippy
  - pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action wasm-check
- 结果：全部通过；live 测试仍按设计忽略。

## 必须修复

### P0 — 事件数据仍可直接持久化凭据材料

位置：NoManCode/rust-app/src/store.rs:248-265、NoManCode/rust-app/src/repository.rs:446-488

Store::event 和 Repository::append_event 直接把任意 data 序列化写入 events。reject_record_secrets 只检查项目名、标题、角色、request_hash 和幂等 key，完全没有覆盖事件 data、旧 Task JSON、工具结果或输出。

例如包含 provider token 的 delta、tool_result 或 error 可以进入事件数据库。报告中“新表和事件不保存 API Key”的结论因此没有实现保证。检测 sk-、dpapi、bearer 也不能覆盖任意 provider/token 格式。

要求：

1. 在 Repository 边界增加明确的事件数据安全策略：拒绝不安全字段，或使用统一脱敏函数后再写入。
2. 对 data 递归处理字符串值，至少覆盖 token、authorization、key、secret 等字段和常见 bearer 格式。
3. 不能只依赖字段名；工具参数、输出和错误文本也要脱敏。
4. 增加测试：delta、tool_result、error、嵌套 JSON 和非 sk- token 均不得在 events/tasks 持久化明文。
5. 该策略要提供给后续 Engine/Approval/HTTP 使用，不能让后续员工绕过它。

### P1 — idempotency_records 的 legacy_run_id 写错

位置：NoManCode/rust-app/src/repository.rs:294-303

Repository::commit_turn 把 turn.session_id.0 写入 idempotency_records.legacy_run_id。但 Session 明确允许 legacy_run_id 与 SessionId 不同，Store::commit_turn 下面的旧表记录使用的是 session.legacy_run_id。这会让新幂等表的兼容投影失真。

要求：

- 让 commit_turn 接收并写入真实的 Session.legacy_run_id，或在事务内从 sessions 表读取；
- 增加测试：Session ID 与 legacy_run_id 故意不同，验证两张幂等表都保存真实 legacy run ID；
- 同时说明旧 idempotency 表和新 idempotency_records 的查询优先级，避免旧 key 造成 UNIQUE 错误而不是明确冲突。

### P1 — EventEnvelope 反序列化不会自动填充 cursor

位置：NoManCode/rust-app/crates/protocol/src/lib.rs:180-198

文档和测试名称声称 legacy envelope 会填充 cursor，但 serde default 只会得到空字符串。旧事件反序列化后 seq=7、cursor=""。只有 domain::Event::envelope() 另外补了 cursor，protocol 层本身没有稳定行为。

要求：

- 实现自定义反序列化，或提供唯一的构造/规范化函数，使缺失 cursor 时始终变为 seq.to_string()；
- 增加 protocol 测试，直接反序列化旧 JSON 后断言 cursor == "7"；
- 对非空 cursor 校验它与 seq 一致，避免伪造或倒序游标。

### P1 — 事件 seq/cursor 可由调用者注入不一致值

位置：NoManCode/rust-app/src/repository.rs:446-466

当 event.seq > 0 时，代码接受调用者提供的 seq 和任意 cursor，没有验证 seq 是否大于当前最大值、cursor 是否等于 seq、schema_version 是否有效。这与 seq 是单调游标、cursor 是 seq 的十进制字符串的协议冲突。

要求：

- 新写入事件统一由数据库分配 seq；
- 或在插入前强制验证 seq/cursor/schema_version，并测试重复、倒序和伪造 cursor 都被拒绝；
- 保留旧事件读取兼容，但不要允许新调用者破坏游标。

### P1 — 恢复状态和事件不是同一事务

Store::recover_turns 先提交 turn/task 状态，再单独调用 self.event。若事件写入失败，数据库会出现已变成 interrupted 但没有对应状态事件的记录。建议把状态更新和恢复事件放入同一个事务，并增加注入失败测试。旧 runs/tasks 恢复路径也应逐步使用同一策略。

### P1 — TurnTask 没有稳定依赖关系

TurnTask 表和结构没有 depends_on/task-ID 字段或关系表，旧 TaskSpec 仍按 display name 保存。报告宣称“依赖身份使用任务 ID”，但实现没有落地。必须补充依赖关系表或明确推迟，并禁止把未实现能力写成完成。

### P1 — 领域关系和状态转换缺少 Host 校验

insert_turn_tx、insert_turn_task_tx 没有校验 turn/session/project/agent 归属，opening_event 的 session/turn/task 链接也没有校验；status 更新也没有状态转换规则。后续 Engine/HTTP 接入前需要补齐，否则任意内部调用都能写跨项目数据或非法跳转。

### P1 — 幂等 replay 契约不完整

Store::commit_turn 对同 key 同摘要只返回 Ok(())，不返回原 Turn；调用者如果继续使用自己新生成的 ID，可能把一次重试误当成新对象。Repository 也没有把旧 idempotency 表和新 idempotency_records 统一查询。必须定义并测试 replay 返回原 Turn，以及旧 key 的 Same/Conflict 行为。

## 建议修复

### P1 — Project/Session/Agent 创建不是原子操作

insert_project、insert_session、insert_agent 是独立操作；只有 Turn 提交是事务。创建流程中途失败可能留下孤立 Project 或 Session。下一轮接入 Engine/HTTP 前，至少提供一个原子创建入口，或明确这些对象只允许由一个组合事务创建。

### P2 — 输入约束还不完整

Project path、名称、ID、Session title 和幂等 key 没有统一长度、控制字符和路径语义校验。最终 Host 边界必须再次校验，不能只依赖前端。

### P2 — 事件 schema 仍允许任意 kind 和任意 data

保留字符串 kind 兼容历史是合理的，但核心事件应有固定 envelope 校验；扩展事件需要明确 namespace，避免后续插件写入未定义事件。

## 放行条件

A 修订后必须：

1. 解决 P0 事件凭据持久化边界；
2. 修复 legacy_run_id；
3. 修复 EventEnvelope cursor 规范化；
4. 限制新事件 seq/cursor 写入；
5. 明确 TurnTask 依赖字段是本轮实现还是延期；
6. 为以上每项增加回归测试；
7. 重新运行完整 Rust 门禁；
8. 更新报告，明确哪些工作仍未接入 Engine/HTTP。

在这些条件完成前，不向员工 B 发放“契约已冻结”的放行结论；B 可以阅读当前实现，但不得基于未修复的事件和幂等语义扩展 Engine。
