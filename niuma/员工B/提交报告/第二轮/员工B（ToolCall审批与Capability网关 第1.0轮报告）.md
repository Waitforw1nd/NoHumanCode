# 员工B：ToolCall审批与Capability网关 第1.0轮报告

2026-09-25；提交人：员工 B；任务 B-R2-01 / NEXT-02C 修订2；执行标识 `B-R2-01-20260925-0232`。

候选实现已提交，等待经理审查。**AP12 未通过，不能验收或合并：全仓补跑 198 通过、5 失败、1 项付费 live 忽略。** 新增审批集成测试 29 项、审批单元测试 2 项通过；标准 check / fmt / clippy / wasm-check 通过，标准 test 失败。未修改旧测试来放宽门槛。

## 1. 受审版本与接续事实

| 项目 | 实际记录 |
| --- | --- |
| 实际模型 | Codex / GPT-6；本会话没有暴露更细模型标识，不声称为 Grok |
| 原接手时间 | 2026-09-25T02:32:07，沿用原记录 |
| 修订2实际接收 | 2026-09-25T06:05:29+08:00；读取完整修订2及经理契约补充审查，未另开重复任务 |
| 开发基线 | `40da4903ab603d805bc1671a148b39aa86b0b7fc` |
| 候选完整 SHA | `50de5456b8c0d412aff4e90821bdde221f066a2d` |
| 分支 / 工作树 | `codex/b/approval` / `../nhc-b-approval/`，路径相对于共享仓库根 |
| Git 状态 | 候选已提交在任务分支，源码工作树干净；未 push、合并、发布，未操作共享仓库暂存区 |
| 源码范围 | 工作树 `NoManCode/rust-app/` 下 approval.rs、engine.rs、repository.rs、store.rs、lib.rs 仅模块导出、新 tests/approval_gate.rs，共六处 |
| 档案位置 | 共享仓库 `niuma/员工B/`；不编辑任务工作树内 niuma 副本 |

继承了 engine/lib/repository/store 四处已跟踪在途修改及未跟踪 approval.rs；接收时没有 approval_gate.rs。未 reset、clean、stash 或覆盖其他人修改。[继承字节指纹](B-R2-01证据/继承修改-sha256.json)和[候选六文件字节指纹](B-R2-01证据/候选源码-sha256.json)分别保留。所有测试发生在提交前，最终源码字节与候选一致；详细时间见各验证 JSON。没有先前第1.0报告，本次新增，未覆盖历史报告。

有效契约：[修订1](../../任务/第二轮/审批与Capability网关契约.md)、[修订2](../../任务/第二轮/审批网关契约补充修订2.md)、[经理契约审查](../../../项目经理/审查记录/2026-09-25NEXT-02C契约补充审查.md)。修订2仅替换明确列出的修订1条款；此次为契约补足，不记作此前未交付实现的失败判定。

## 2. 实现及持久化边界

调用关联使用 NOT NULL 的 `(task_id, tool_call_id)` 复合唯一键。Engine 对整组新调用预检：空 id、组内重复、复用同任务历史 id 均在本组建卡、backup、执行之前失败。跨任务同名 id 各自审批；resume 读取同一条已持久消息不会另建卡，历史配对歧义拒绝恢复。

参数摘要为规范化 JSON 的 SHA256 小写十六进制：递归对象键排序，数组保留顺序，标量遵循 serde_json，不承诺 `1` 和 `1.0` 等价。绑定摘要是以下结构化 JSON 再规范化并 SHA256，字段没有无分隔拼接：

```text
version = "approval-binding-v2"
task_id, session_id (nullable), turn_id (nullable), tool_call_id, tool_name,
workspace (持久化标识), write_scopes (有序数组), allow_commands,
args_digest (规范化执行参数摘要)
```

领取执行权前重新读取持久任务、审批归属、调用历史、工具 definitions 和当前 capability policy，并比较工作区、权限与参数绑定。只修改摘要以外的元数据也会拒绝。Unknown 工具默认 Deny，不能因进入 definitions 而直通。list/read/search 及当前无 host import 的 run_wasm 直通；write_file 和 run_command 必须逐次审批，现有 workspace 防线仍执行。

审批行不保存原参数。write_file preview 只有安全相对路径和正文 UTF-8 字节数；run_command 脱敏并处理环境引用后按 Unicode 字符截到 160。副作用工具的 tool_start 只保存 name、调用/审批 id 和参数摘要，不保存完整 arguments 或正文。approval 事件走既有 redact_persisted；file_backup 仍沿用既有脱敏备份设计，会保存适用脱敏后的旧文件内容。无法由持久消息恢复原参数（包括脱敏导致 JSON 不能解析）时返回 BindingConflict，不执行替代值，也没有新增明文秘密快照。

决定与执行分离：pending→approved/denied 的条件更新及 resolved 同事务；批准后仍需独立事务领取 not_started→claimed，提交成功后才 backup/tool_start/execute。工具返回后，安全 tool_result、tool 消息、task/turn 投影与 finished 在同一事务提交。工具真实错误可以作为 error 结果 finished；提交失败保持 claimed，recover 才改为 unknown。finished 查询验证对应消息及结果事件，缺失或不相符返回 CorruptState。

取消用与 claim 相同的 SQLite Immediate 事务边界裁决。保存任务取消事实、pending→cancelled 及 resolved；approved+not_started 只改 execution_state=cancelled，保留原决定且不发第二条 resolved。后台保存不能用过期 completed 覆盖已持久 cancelled。claim 已提交时不能撤销副作用，真实结果仍可原子写入，下一工具被阻止。等待者注册后重读数据库，每次唤醒重读，并以 100ms 数据库检查观察其他 Store 的决定；通知不作为唯一事实源。

recover 保留 pending，running/queued→interrupted，claimed→unknown。unknown 不自动重试、不补造成功、不再向模型自动请求重复执行。resume 按原调用顺序补齐结果，再进入新输入的 provider 循环。重启后无等待者的 decide 只落库，不 launch。

schema 6→7 的 DDL、迁移标记与 user_version 在同一事务；schema6 原校验不放宽。检查真实列形状、复合唯一约束、索引列、完整规范化表 DDL（含 CHECK/FK）及行关系。此开发版本对不匹配的未发布 schema7 明确拒绝；没有删除或自动重建旧开发库。夹具只在 TempDir 创建，schema6 fixture 由现行数据库移除仅属7的审批表/标记并设回6构造，保留实际 schema6 表和预置数据；没有声称迁移过生产用户数据库。

## 3. AP01～AP20 逐项证据

测试均在授权新文件 `NoManCode/rust-app/tests/approval_gate.rs`；完整29项名称和结果见[最终全仓日志](B-R2-01证据/regression.log)。下表“通过”表示候选自测，不代替经理独立验收。

| 编号 | 测试 / 实际断言 | 本轮结果 |
| --- | --- | --- |
| AP01 | `ap01_ap02_pending_then_approved_executes_once`：pending/requested；目标不变，无 backup/start/result；preview 安全 | 通过 |
| AP02 | 同测试：批准后一次真实写入、resolved、决定人/时间、真实结果及 finished；不保存正文参数到 start | 通过 |
| AP03 | `ap03_ap09_denied_and_decision_conflicts`：deny 后文件不变、安全 tool 错误、继续完成 | 通过 |
| AP04 | `ap04_read_tools_pass_through`：list/read/search 正常结果，无审批行/requested | 通过 |
| AP05 | `ap05_command_waits_and_runs_once`：真实 PowerShell 命令；approve/deny 两分支，批准执行一次，拒绝无执行；先等待 | 通过（最终全仓运行包含双分支） |
| AP06 | `ap06_ap07_ap18_runtime_restart_and_decide_before_waiter`、`ap06_reconcile_preserves_order_with_pending_and_unapproved_calls`：真正停止 runtime，保留 pending，批准后恢复一次；结果按调用顺序 | 通过 |
| AP07 | AP06 共用重启测试 deny 分支：重开后拒绝，安全结果，无文件副作用，可继续完成 | 通过 |
| AP08 | `ap08_cancel_pending_is_durable`：cancelled/resolved 持久，等待者醒来、零执行、任务 cancelled | 通过 |
| AP09 | `ap03_ap09_denied_and_decision_conflicts`、取消测试：不存在与已决定 typed 错误，重复决定不增事件 | 通过 |
| AP10 | `ap10_ap11_safe_previews_independent_calls`，单元 `digest_is_stable_and_preview_omits_secret_and_body`，AP14脱敏恢复测试：扫描 mock 秘密/正文/arguments/env，摘要稳定及变化 | 通过 |
| AP11 | `ap10_ap11_safe_previews_independent_calls`：两卡独立，首个批准、第二个拒绝，仅首个文件变化 | 通过 |
| AP12 | AP19迁移测试通过；基线至候选仅授权六处、lib仅一行。标准全仓test失败；补跑5项旧测试失败，详见第6节 | **未通过，阻塞验收** |
| AP13 | `ap13_cross_task_same_call_isolated`、`ap13_duplicate_groups_have_no_effect`、`ap13_empty_id_and_ambiguous_history_fail_closed`：跨task隔离、组重复/后续复用/空id零本组副作用，歧义历史拒绝 | 通过 |
| AP14 | `ap14_binding_changes_rejected_without_new_events`、`ap14_persisted_workspace_and_permission_changes_conflict`、`ap14_redacted_arguments_cannot_resume_as_substitute`：path/command/工作区/权限/绑定错配拒绝，不增卡或resolved；键序等价；脱敏参数重启不执行替代值 | 通过 |
| AP15 | `ap15_approved_before_claim_stopped_runtime_executes_once`、`ap15_claimed_before_execute_runtime_stops_then_unknown`、`ap15_real_command_effect_then_runtime_stop_unknown`：claim前三者边界详见下文；后两者计数0/1且unknown，禁止resume与伪完成 | 通过，夹具边界见下文 |
| AP16 | `ap16_finish_transaction_fault_rolls_back`、`ap16_public_finish_failure_preserves_all_transaction_inputs`、`ap16_finished_restart_never_reexecutes_and_missing_result_is_corrupt`：真实写入后完成事务故障回滚；task JSON/事件/claimed一致，重开unknown；真实finished重启不重做，删结果即损坏 | 通过 |
| AP17 | `ap17_multistore_decision_cancel_race`、`ap17_multistore_cancel_claim_barrier`、`ap17_claim_cancel_order_and_stale_completed`、`ap17_cancel_after_real_claim_preserves_real_result_and_stops_next_call`：屏障、多Store；一次决定/resolved，取消先于claim拒绝，claim先保留结果并阻止下一调用 | 通过 |
| AP18 | AP06重启测试、`ap18_decision_and_cancel_from_other_store_are_observed`、`ap18_cancel_before_new_waiter_resume_has_safe_cancel_result`：决定/取消在新等待者前落库；无等待者不launch；跨Store仍能有界观察 | 通过 |
| AP19 | `ap19_schema6_preservation_atomic_migration_and_forged_constraints`、`ap19_constraints_enforced_and_partial_ddl_retry`：schema6行保全、标记故障DDL/version回滚、部分DDL重入、缺CHECK和伪索引拒绝、约束真实生效；未改旧测试 | 通过 |
| AP20 | `ap20_public_query_order_and_corruption_distinct_from_missing`及AP09/AP10/AP14：公开查询隔离和确定序、not-found≠损坏、typed错误、安全扫描；单元 `classification_defaults_unknown_tools_to_approval_deny` | 通过 |

AP15 采用真实 `Runtime::shutdown_timeout(Duration::from_secs(2))` 后再建 runtime/重开数据库，非 drop Arc。claim 前用单线程 runtime，在同步 decide 后不再让出执行，停止后零副作用，resume 执行一次。claim 后执行前使用 SQLite BEFORE file_backup trigger 抛错，证明真实 Engine 已持久 claim、未进入 execute，再真正停止 runtime；这不是在 CPU 任意指令处强杀进程。命令执行后结果提交前则真实 PowerShell 追加一行计数再长时间等待，测试观察完整一行后停止 runtime，现有 kill_on_drop 取消命令，重开仍只有一行、无成功结果、非 completed。未做操作系统断电实验。

AP16 的 BEFORE finished UPDATE trigger 在消息/事件写入后故障；事务回滚后 Engine 可另行保存真实 failed 状态，该后续保存不能误说成完成事务半提交。额外公开 finish 测试对比整个 task JSON 和事件数，验证完成事务所有输入保持。AP17 两类竞争各12轮，分别使用3/2个 Store 连接和屏障；AP18 用持久前置条件和有界 yield 检查，不靠随机 sleep 赢得竞争。

## 4. AG逐项回应

| 编号 | 实现与证据 | 当前结论 |
| --- | --- | --- |
| AG-01 | task+call复合键、整组预检及历史验证；AP13 | 候选自测通过，待审 |
| AG-02 | 参数/权限/工作区绑定，执行前重查，不能恢复则拒绝；AP14 | 候选自测通过，待审 |
| AG-03 | claim先持久化、完成同事务、恢复unknown/finished校验；AP15/AP16 | 候选自测通过，待审 |
| AG-04 | 决定/取消/claim事务、注册后重读和多Store检查；AP17/AP18 | 候选自测通过，待审 |
| AG-05 | schema6保全迁移、真实约束与开发7拒绝；AP19 | 新迁移证据通过；AP12旧回归失败未解除 |
| AG-06 | Unknown Deny、安全record/typed错误/查询；AP20及下列真实接口 | 候选自测通过，供C审阅，尚未验收冻结 |

## 5. 真实公开接口（候选版，供后续HTTP审查）

以下 `Result` 均为 `anyhow::Result`，`Task` 为现有 domain::Task，`Value` 为 serde_json::Value。没有新增HTTP/CLI/UI入口。未来传输层宜调用 Engine 决定入口及安全 Store 查询；建卡/claim/finish 是应用执行内部协调接口，不能给远程请求直接调用以绕过 Engine。

```rust
// Engine
pub fn decide_approval(&self, approval_id: &str, approved: bool,
    decided_by: Option<&str>) -> Result<approval::ApprovalRecord>;

// Store
pub fn approval_by_tool_call(&self, task_id: &str, tool_call_id: &str)
    -> Result<Option<approval::ApprovalRecord>>;
pub fn approval(&self, id: &str) -> Result<approval::ApprovalRecord>;
pub fn approvals_for_task(&self, task_id: &str)
    -> Result<Vec<approval::ApprovalRecord>>;
pub fn pending_approvals_for_task(&self, task_id: &str)
    -> Result<Vec<approval::ApprovalRecord>>;
pub fn ensure_approval(&self, task: &Task, tool_call_id: &str,
    tool_name: &str, args: &Value) -> Result<(approval::ApprovalRecord, bool)>;
pub fn decide_approval(&self, approval_id: &str, approved: bool,
    decided_by: &str) -> Result<approval::ApprovalRecord>;
pub fn cancel_pending_approvals(&self, task_id: &str)
    -> Result<Vec<approval::ApprovalRecord>>;
pub fn claim_approval(&self, task: &Task, tool_call_id: &str,
    tool_name: &str, args: &Value) -> Result<approval::ApprovalRecord>;
pub fn finish_approval(&self, task: &Task, approval_id: &str, result: &str,
    name: &str, tool_call_id: &str) -> Result<()>;

// approval 模块
pub fn classify(tool_name: &str) -> ToolEffect;
pub fn evaluate(task: &Task, tool_name: &str) -> PolicyDecision;
pub fn args_digest(args: &Value) -> Result<String>;
pub fn binding_digest(task_id: &str, session_id: Option<&str>,
    turn_id: Option<&str>, tool_call_id: &str, tool_name: &str,
    workspace: &str, write_scopes: &[String], allow_commands: bool,
    args: &Value) -> Result<String>;
pub fn preview(tool_name: &str, args: &Value) -> String;
pub fn tool_error(status: ApprovalStatus) -> String;
```

Engine decided_by=None 默认为 user。无在程等待者时不启动执行；查询全部/待定均 `(created_at,id)` 排序，未知task列表为空。ensure返回二元组中的bool表示是否新建。既有 resume/cancel 签名不变；没有重置 unknown 自动重试 API。

```rust
pub struct ApprovalRecord {
    pub id: String,
    pub tool_call_id: String,
    pub task_id: String,
    pub turn_id: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: String,
    pub args_digest: String,
    pub binding_digest: String,
    pub workspace: String,
    pub write_scopes: Vec<String>,
    pub allow_commands: bool,
    pub preview: String,
    pub status: ApprovalStatus,
    pub created_at: u64,
    pub decided_at: Option<u64>,
    pub decided_by: Option<String>,
    pub execution_state: ExecutionState,
}
pub enum ApprovalStatus { Pending, Approved, Denied, Cancelled }
pub enum ExecutionState { NotStarted, Claimed, Finished, Unknown, Cancelled }
pub enum ApprovalError {
    NotFound { id: String },
    Conflict { id: String, status: String },
    BindingConflict { id: String },
    UnknownResult { id: String },
    CorruptState { id: String },
}
pub enum ToolEffect { None, WriteFs, ExecCommand, Unknown }
pub enum PolicyDecision { Allow, RequireApproval, Deny { reason: &'static str } }
```

两个状态枚举 serde 使用 snake_case，均有 `as_str(self) -> &'static str` 和 `parse(value: &str) -> Option<Self>`。ApprovalError 实现 std::error::Error 和固定安全 Display；通过 anyhow downcast/error chain 分类，不解析文案。原契约的语义名 ApprovalNotFound / ApprovalBindingConflict 在真实实现中分别为 ApprovalError::NotFound / ::BindingConflict。仍可能返回其他存储/策略错误，HTTP必须安全处理未分类内部错误；不得将所有错误伪装成404或把 Debug 内容直接发给客户端。

## 6. 实际验证命令、退出码与范围外依赖

真实环境：PowerShell **7.6.5**，rustc `1.98.1 (48a229cea 2026-09-01)`，cargo `1.98.1 (797e8a9bc 2026-08-05)`，stable-x86_64-pc-windows-msvc，已安装 wasm target。本轮发现的 pwsh 确为7，不沿用历史 PS5.1 shim 结论。

第一次标准check因MSVC发现失败退出1；直接cargo check也因缺cl.exe失败退出1。定位现有MSVC 14.40.33807后，在调用进程设置 VSINSTALLDIR 指向本机已安装VS，再执行未修改的 build.ps1 成功。本机安装绝对位置不写入仓库；未修改构建脚本、Cargo或安装依赖。可移植[验证脚本](B-R2-01证据/验证.ps1)从自身位置定位共享仓库及同级工作树，由调用方提供VS环境。

以下命令工作目录均为任务工作树 `NoManCode/rust-app`。五个标准动作分别调用；补充命令在同一进程先标准check初始化编译环境，结果亦记录在其日志中。

| 实际命令 | 退出码 / 结果 | 日志 / 起止时间记录 |
| --- | --- | --- |
| `./build.ps1 -Action check` | 0，通过 | [log](B-R2-01证据/check.log) / [JSON](B-R2-01证据/check.json) |
| `./build.ps1 -Action test` | 101，60通过/1失败，随后停止 | [log](B-R2-01证据/test.log) / [JSON](B-R2-01证据/test.json) |
| `./build.ps1 -Action fmt` | 0，通过；成功时无标准输出，未生成log文件 | [JSON](B-R2-01证据/fmt.json) |
| `./build.ps1 -Action clippy` | 0，通过 | [log](B-R2-01证据/clippy.log) / [JSON](B-R2-01证据/clippy.json) |
| `./build.ps1 -Action wasm-check` | 0，通过 | [log](B-R2-01证据/wasm-check.log) / [JSON](B-R2-01证据/wasm-check.json) |
| `cargo test -p peachsh --locked --test approval_gate` | 0，29通过 | [log](B-R2-01证据/approval.log) / [JSON](B-R2-01证据/approval.json) |
| `cargo test -p peachsh --locked --lib approval::` | 0，2通过 | [log](B-R2-01证据/approval-lib.log) / [JSON](B-R2-01证据/approval-lib.json) |
| `cargo clippy -p peachsh --lib --test approval_gate --locked -- -D warnings` | 0，通过 | [log](B-R2-01证据/approval-clippy.log) / [JSON](B-R2-01证据/approval-clippy.json) |
| `cargo test --workspace --locked --no-fail-fast` | 101，198通过/5失败/1忽略 | [log](B-R2-01证据/regression.log) / [JSON](B-R2-01证据/regression.json) |
| `git diff --cached --check`（提交前） | 0，通过；六路径白名单、无未暂存差异 | 候选SHA及源码指纹可复核 |

失败运行的工具外层shell报告 exit1；上表101是脚本捕获到的 cargo/build `$LASTEXITCODE`，两者不混写。定向 approval 日志早于最后一次 AP05 增加 deny 分支；最终 regression 在该测试变更后，实际运行全部29项并通过。最终 check/test/fmt/clippy均在最后源码变更后；wasm-check较早，后续变化仅Host Engine及审批测试，不涉及其WASM输入，未重复计算。日志旧版本以 previous 时间戳保存，没有删失败记录。

五项失败如下，构成 AP12 阻塞：

| 失败测试 | 证据及与新契约的冲突 | 经理需决定的范围 |
| --- | --- | --- |
| `store::tests::interrupted_schema_6_retries_without_advancing_early` | src/store.rs 旧测试断言 schema等于6，实际7 | 允许迁移该旧测试版本预期，仍保留断点回滚/数据校验 |
| `http_contract::h1_read_shapes_and_security_headers_stay` | tests/http_contract.rs:485 固定6，实际7 | C或指定写入人调整schema响应预期，保留安全header断言 |
| `runtime::dependency_order_summary_and_cycle_rejection` | tests/runtime.rs:148 等待完成超时；写工具现在停在pending | 授权旧runtime夹具显式逐次批准，保留依赖/环断言 |
| `runtime::replace_existing_file_preserves_hardlink_and_command_permission` | 同等待超时，未发审批决定 | 授权适配审批步骤，保留硬链接与命令权限断言 |
| `runtime::tool_execution_and_path_rejection` | 同等待超时，未发审批决定 | 授权适配审批步骤，保留真实执行和路径拒绝断言 |

这是候选实际全仓失败，不能计为通过，也不能仅凭原因分析认定适配后必然全绿。契约禁止修改旧测试，包括store.rs中已有测试；因此未擅自修改，曾继承的schema断言改动已恢复为基线。建议经理修订旧测试写权及责任人，适配后重新跑标准test和相关回归；当前不扩大B源码范围。

开发中还实际出现过：初次审批集成15过4败（秘密脱敏破坏JSON、命令计数文件创建早于完整写入、损坏夹具FK仍启用）；随后修正恢复检查、完整计数观察及单故障夹具，最终通过。继承单测局部preview变量遮蔽函数曾触发E0618，已修正。初次fmt不通过后已格式化授权文件；一次rustfmt从错误目录运行报找不到文件后改在Rust目录执行成功。这些早期输出见会话工具记录，未保存为原始附件，不补造退出码/时间。环境和开发失败没有冒充最终通过日志。

未运行：发布build、付费live、真实HTTP审批端点（本轮不存在）、OS断电/独立进程强杀实验、正式用户库迁移、经理隔离复验。paid live保持ignore。源代码仅六处差异，既有测试正文未改变；档案新增只在共享B报告/证据及身份，不触碰经理/C在途修改。

## 7. 已知限制、未完成与交回经理

- 外部副作用与SQLite没有共同事务，只提供保守的最多一次自动尝试。claim后执行前崩溃可以是零次执行并unknown；不做租约重试，人工核查后必须新调用/新审批。
- 审批不是OS沙箱，未解决文件系统外部修改、符号链接等全部TOCTOU；保留workspace现有防线。
- 脱敏后原参数不可还原的调用不能跨重启执行，需要用户检查后重新请求。未新增秘密存储或绕过DPAPI。
- 等待审批仍占用slots/key_slots，没有审批过期、批量永久授权、HTTP/UI/CLI。批准不主动恢复停下的任务。
- 已claim的工具取消后仍可能产生真实副作用；保存真实结果或恢复unknown，不能宣称取消撤销副作用。
- resume的新输入在审批reconcile完成前暂存Engine内存；已有resume事件持久化，但若此间再次崩溃，需用户重新明确提供resume输入，未承诺自动恢复这一暂存输入。
- 不兼容的未发布开发schema7明确拒绝，不删除现场。测试重建仅针对可丢弃TempDir夹具，真实用户schema6必须走保全迁移。
- **未完成：AP12全仓门禁、经理独立审查与验收、后续API冻结/整合。** 下一步第一操作：经理读取本报告和候选SHA，审查安全实现及五项失败，为旧测试适配明确范围和责任人。B等待审查返工，不自行修改旧测试或传输层。

本次提交是候选交付，**不是验收通过、合并或发布**。C可据本报告了解真实API，但应等待B验收整合及接口冻结后再按有效派发实现HTTP。
