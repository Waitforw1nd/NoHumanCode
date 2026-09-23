> **现行状态补充（2026-09-23，项目经理）：第一轮已按范围验收结束。** 下文是历史任务原文，其中“可分发执行”“直接实施”“批准并行”等只代表当时安排，不授予本次新任务权限。
> 新对话使用 [员工启动提示词](../../../../员工启动提示词.md)，先读 [AGENTS](../../../../AGENTS.md)、[本人身份](../../个人身份认知.md) 和 [最终审查](<../../审查记录/第一轮/员工B 最终审查.md>)；仅查证历史时无需开工。有明确新任务时按当前派发范围执行。

# 员工 B 第一轮提示词：Engine 连续 Chat 回合

状态：负责人已确定本轮范围，可分发执行。日期：2026-09-23。  
从下一节开始可完整交给 Grok 4.7。下面是开发任务，不是只写方案的请求。

并行安排补充（2026-09-23）：员工 C 可同步完善现有 HTTP/SSE 传输契约，仅写 `src/server.rs`、新增 `tests/http_contract.rs` 及其文档；C 不引用本轮未验收的新命令，也不挂载新的 Turn 写路由。B 保持本文件原定所有权，不改 C 的文件。两人只格式化自己拥有的文件，全量格式检查仍可运行；联合验收前不声称对方能力已通过。详细边界见 `niuma/员工C/任务/第一轮/员工C提示词.md`。

## 你的角色与任务

你是 Grok 4.7，员工 B，负责 🍑sh harness 的 Engine 应用服务。项目负责人负责方向、公共契约和最终 review；你负责本任务范围内的实现、调试、测试和交付报告。

项目目录：`./`。先读完必读资料和代码，核对当前状态，然后直接实施。已确定的设计不需要再次逐项向用户确认；普通实现细节由你完成。

**本轮目标：让一个已经存在、已完成首轮的无工具 Chat Session 接收新的用户消息，每条新消息形成新的 Turn 和 Task，复用同一个 Project、Session 和 Agent，保留旧轮次快照，并可靠处理重试、并发和重启。**

只交付 Engine 应用服务和必要 Store 支持，供后续 HTTP/CLI 复用。不要把现有 `resume` 改名后当作新回合实现。

## 一、必读资料与有效基线

依次读取：

1. `./项目开发守则.md`
2. `./PROJECT-BOOK.md`
3. `./NoManCode\rust-app\MASTER-ARCHITECTURE-BASELINE-2026-09-22.md`
4. `./niuma\员工A\审查记录\第一轮\员工A 最终审查.md`
5. `./niuma\员工A\复盘\员工A复盘与反思日志.md`
6. `./niuma\项目经理\开发记录\后续开发注意事项.md`
7. `NoManCode/rust-app/src/engine.rs`、`store.rs`、`repository.rs`、`domain.rs`、`crates/protocol/src/lib.rs`、`tests/final_acceptance.rs` 和 `tests/runtime.rs`。

开始先记录 `git status --short` 与 `git rev-parse HEAD`。A 已通过的最终修复尚未提交，不能 reset、覆盖或只检出 `8f0ccdc` 后开工。若你拿到的是其他工作区，先核对上述基础是否存在，缺失时报告基线问题，不能假装继承完成。

A 已完成的内容不要重做：新建时的 `commit_turn_bundle`、Engine 首次创建、统一幂等、ID 依赖、状态/事件事务、恢复、基础领域查询和 Session SSE。上轮实际门禁为 71 通过、1 个 live 忽略；你必须记录自己本轮的实际结果。

## 二、负责人已确定的产品与领域语义

### B-01：新消息创建新回合

- 仅支持已经存在的 `SessionKind::Chat`，一个 Agent、一条无依赖的可执行任务。
- 请求明确给出 `session_id`、`agent_id`、`expected_last_turn_id`、`message`、`idempotency_key`。
- ProjectId、SessionId、AgentId 和 Session.legacy_run_id 不变；成功新建时生成新的 TurnId、TaskId 和对应旧 Task 行。
- 新旧任务投影仍写入同一个 legacy Run。不要把 SessionId 当作 legacy_run_id，也不要为每条消息再创建 Session/Agent。
- 上一轮内容、输出、usage、状态保持不变；当前轮的 output 从空开始、usage 从零/空开始，不能继承上一轮成本或回答作为本轮结果。
- 新命令返回与该请求匹配的具体 Turn/Task 和 `replayed` 信息，不用整个 `Run` 或 `run.tasks[0]` 代替本轮结果。

建议公开 Rust 入口为 `Engine::send_chat_turn(command)`。可在 domain 定义 typed command/result，字段使用现有强类型 ID。方法名可以微调，字段语义和行为不可变化。本轮不要求定义新的 HTTP 路由或搬迁全部 DTO。

### B-02：首轮明确限制

- 最近提交的 Turn 及其唯一 Task 必须都为 `completed`，且请求中的前序 ID 正是该 Turn。
- queued/running 或其他非 completed 状态不能追加新回合；失败、取消、中断先走已有显式 resume。不要自动恢复或绕过未完成轮次。
- 只接受原本无工具的 Chat：上一轮 Task 的 `tools=false`、`allow_commands=false`、`write_scopes=[]`、无任务依赖；新增 Task 继续维持这些限制。
- 带工具权限、Team、Plan、legacy-only 无 Session 映射、空历史、多 Agent/多 Task 或损坏归属明确拒绝。本轮不静默关闭权限来“兼容”不支持的输入。
- 用户消息不能为空或全空白，沿用 100,000 字节上限；key 使用现有 1～200 可见 ASCII 字节校验。换行是正常消息内容，不按 ID 规则过滤正文。
- 不实现分支/编辑历史、换模型、自动摘要压缩或完整聊天 UI。

### B-03：保持模型、项目和上下文

- 从上一轮持久化 Task 取得 Route/model 和 workspace 快照。核对它属于 Session 的 Project；不因全局 Settings.workspace 改变而切换工作目录。
- 继续通过 `Engine::key` 获取凭据，保留路由删除、API 地址变化的保护；不得把新 Key 发送到已不匹配的旧地址。新一轮不支持显式更换模型。
- 上一轮 Task.messages 已可能包含完整前缀。复制这份合法消息序列，再追加一次本次 user message；不能把所有历史 Task.messages 累加。
- 不把旧 SSE 草稿当作已完成 assistant 消息。上下文有未配对 tool_call、非法序列或超过现有 1,500,000 字节限制时，在新增数据和调用 Provider 前明确失败，不伪造结果、不静默裁剪。
- 新 TaskSpec 的 prompt 对应本次消息，名字/角色维持已有 Agent 身份；显示名不参加会话归属判断。

## 三、并发、幂等和事务要求

### B-04：先回放，再验证追加资格

摘要基于稳定语义输入：命令类型/版本、SessionId、AgentId、expected_last_turn_id、原始 message；不要把随机新 ID、当前时间、可变全局设置或临时运行状态加入摘要。

先按统一幂等边界分类：

- 同 key、同摘要且匹配目标命令：返回原 Turn/Task，即使会话已经创建下一轮，也要正确回放这一轮。
- 同 key、不同消息、Session、Agent 或前序：结构化 conflict。
- key 已绑定不适用的 legacy-only Run 或其他命令：结构化 conflict，不猜造 Turn、不创建第二份数据。
- 回放不调用 Provider，不执行工具，不再次 launch，也不新增状态事件。

保留 `classify_both` 的统一新旧表语义，不自行再实现一套只查新表的判断。公开错误可以复用既有 `IdempotencyConflict`，新增前序过期/会话忙等使用可分类的应用错误；不要让调用者解析中文字符串。

### B-05：以持久顺序确认最新回合

现有 `turns_for_session` 按 `created_at,id` 排序，时间是秒精度，UUID 不表示先后。因此不能用它的 `.last()` 判断最新回合。

本轮批准的最小方案：通过每个 Turn 在创建事务中已经写入的首条事件 `seq` 判定提交顺序，新增必要的 Repository/Store 查询；在同一事务中获取最新回合。缺少首条事件或关联损坏时明确报错，不用时间戳猜测。保持 schema 6，不为此新增序号列或迁移。

这个查询是给本轮命令使用的最小支持，不要求改写现有所有列表接口的排序语义。报告说明此选择及未来列表适配要注意的顺序差异。

### B-06：原子追加，成功创建才派发

在一个 Store 写事务中完成：统一幂等检查 → 检查 Session/Agent/Project、真实最新前序和允许的状态 → 插入新 Turn、新旧 Task、幂等记录和初始事件 → 提交。

可以新增 `append_chat_turn` 一类 Store 命令，复用现有 `commit_turn_in_tx`；不把检查放在事务外再直接 `commit_turn`。采用适当写事务避免两个连接都读到相同前序后各自追加。Engine 内存 gate 不能替代数据库条件检查。

已有 `commit_turn_bundle` 会插入 Session，不适用于追加。Engine 不访问 SQLite 连接或直接 SQL。

两个不同 key 对相同前序并发提交，最多一个新回合成功；另一个明确冲突。同 key 并发重试返回同一个 Turn，只有真实创建成功的一方 launch。模型调用必须发生在数据库提交成功之后。

### B-07：保护历史、恢复和旧入口

- 保留旧 start / runs / Task resume 的基本兼容行为，不擅自删除旧 API。
- 对映射到 Chat 的旧 Task，若所属 Turn 已有后续 Turn，拒绝通过 resume 再修改它的历史快照。此“必须为最新轮次”的检查也进入 Store 的 resume 事务，避免并发绕过。
- 对最新轮次的合法 resume 继续保留 Task/Turn/模型身份；legacy-only 和 Team 的现有语义不因本轮无关重构而变化。
- 新轮次运行中取消或重启，沿用状态/事件事务及草稿恢复。已持久化的 cancelled 保持 cancelled；重启时仍 queued/running 的任务才转 interrupted。重启不自动重新调用 Provider，重复 send 只回放；用户显式 resume 才重新执行该轮。
- 数据库提交后、launch 前的崩溃也遵循上述中断恢复边界。本轮不承诺分布式恰好一次执行或持久 Scheduler。

## 四、修改边界

本轮你是以下文件本任务相关区域的唯一写入人：

| 文件 | 允许修改 |
| --- | --- |
| `NoManCode/rust-app/src/engine.rs` | 新增应用命令、调用既有执行器、必要的兼容保护 |
| `NoManCode/rust-app/src/domain.rs` | 本任务 DTO/应用错误；纠正与实际映射不符的相关旧注释 |
| `NoManCode/rust-app/src/store.rs` | 原子追加、回放结果解析、Chat 最新轮次 resume 保护 |
| `NoManCode/rust-app/src/repository.rs` | 真实提交顺序等必要只读查询；复用已验收提交逻辑 |
| `NoManCode/rust-app/tests/session_turns.rs` | 本轮主要新增集成测试和本机 mock Provider |
| `NoManCode/rust-app/tests/runtime.rs`、`tests/final_acceptance.rs` | 必要兼容回归；不能删除或放宽原验收 |

保留 protocol 现有 ID、状态和错误码语义。本轮 DTO 放 domain 即可；不改 schema、事件 envelope、旧字段含义或 Rust 依赖版本。若确实必须超出这些边界，报告可复现依赖与最小建议，同时推进不依赖它的工作。

不新增 HTTP 写路由，不修改 UI、Provider 协议、secrets、workspace/WASM 工具，不重写审批、插件系统、Team、KeyPool 或 Scheduler。后续由负责人安排 C 接客户端、D 做独立验收。

## 五、必须提供的验收证据

在临时数据库和本机 mock Provider 上验证，使用合成测试凭据，不读取或输出真实 Key。

| 编号 | 场景 | 必须断言 |
| --- | --- | --- |
| T1 | 原 Session 连续 2～3 轮 | Project/Session/Agent/legacy Run 不增加；每次新建唯一 Turn/Task；旧快照不变；新 output/usage 独立 |
| T2 | Provider 消息序列 | 上下文完整且不重复，当前 user 只出现一次，无工具声明，无文件/命令执行 |
| T3 | 同 key 顺序和并发重试 | 同一 Turn/Task，新增对象和事件不增加，Provider 调用次数不增加；后续已有另一轮时仍回放正确旧轮 |
| T4 | 同 key 改消息/Session/Agent/前序，以及旧 Run key | 可分类 conflict；零新增任务、事件和 Provider 调用 |
| T5 | 不同 key、相同前序并发追加 | 至多一个成功；用独立 Store 连接证明不是仅依靠一个 Engine 的内存锁 |
| T6 | 同秒多轮且 UUID 字典序与创建相反 | 真实最新前序判断正确；陈旧前序被拒绝 |
| T7 | 非 Chat、错误 Agent/归属、非法输入、带工具历史、缺失/损坏前序 | 明确拒绝，无孤儿记录，无 Provider 调用；不能仅因夹具其他错误而“通过” |
| T8 | 事件 INSERT trigger 注入失败 | 新 Turn/Task、旧投影、幂等全部回滚，Provider 调用为 0 |
| T9 | 新轮取消、运行中部分输出后重启、提交后未 launch 的中断 | 已落库取消保持 cancelled；重启时 queued/running 转 interrupted，保留草稿和身份；重启和回放不自动调用模型，显式 resume 才继续 |
| T10 | 历史轮次 resume 与新追加竞争 | 历史快照不能重开；最新轮次合法 resume 保留身份；legacy-only/Team 兼容测试仍通过 |
| T11 | route 删除/host 变化、全局 workspace 变化、上下文超长 | 不泄漏凭据到旧地址，不切项目，不默默裁剪；失败无新增调用 |
| T12 | Session 事件读取 | 同 Session 的多个 Turn 归属正确，游标严格向后；用现有 Store/SSE 能力验证，不另造事件链路 |

每条可拆成多个测试。报告对象数、消息序列、错误类别、回滚结果和调用次数，不只给通过总数。测试应可稳定重复，不靠任意长 sleep 猜测执行完成。

最终在 `./NoManCode\rust-app` 执行：

```powershell
pwsh -NoProfile -File build.ps1 -Action test
pwsh -NoProfile -File build.ps1 -Action fmt
pwsh -NoProfile -File build.ps1 -Action clippy
pwsh -NoProfile -File build.ps1 -Action wasm-check
git diff --check
```

真实付费 live 测试沿用显式开启规则，未执行就如实报告。环境缺失不能伪造成功或用另一种 shell 冒充目标环境。

## 六、实施与报告要求

先将上述 B-01～B-07 和 T1～T12 映射为一份简短实施清单，可在最终报告草稿中维护。按已确认契约直接开发，不停留在方案描述。

完成后写入：

`./niuma\员工B\提交报告\第一轮\员工B（Engine连续Chat回合 第1轮报告）.md`

报告必须包括：

1. HEAD、继承的工作区变更、本轮修改文件；
2. B-01～B-07 的实现入口与完成状态；
3. 公开应用命令的真实签名、成功/回放/冲突示例，供后续 C 使用；
4. T1～T12 的证据、实际命令结果和未执行项；
5. 原子追加的事务顺序、最新轮次判定、重启边界；
6. 公共契约、schema、权限和凭据影响；
7. 未完成项、风险与负责人需要决定的事项。

不要自行写“审查通过”。负责人将把结果和返工思路写到 `./niuma\员工B\审查记录\第一轮\员工B review.md`。本轮不自动发布、替换运行中的实例或提交继承自他人的全部改动。

完成后只需向用户简洁说明交付、测试结果和报告路径，等待负责人 review。
