# B-R2-01：ToolCall 逐次审批与 Capability 网关（Host 应用层）

日期：2026-09-25；修订1；全局切片NEXT-02C；项目NoHumanCode，根 `./`，源码 `NoManCode/`，Rust目录 `NoManCode/rust-app`。

**执行角色：员工B；执行模型由用户分发时确定（默认 Grok 4.7 执行线）。** 经理负责契约与review，员工负责实现、调试、正式测试；用户分发提示词。状态：经理已准备、待用户分发，尚无执行标识，不宣称已开工。

来源：任务板"NEXT-02后续"（插件生命周期→再接审批工具）、[项目书](../../../../PROJECT-BOOK.md) §3.4（副作用可审阅）与 §5（写入、命令和网络都经过审批）、架构基线 `MASTER-ARCHITECTURE-BASELINE-2026-09-22.md` 的 ToolCall/Approval 语义（批准绑定工具、参数、路径/命令、工作区和会话；参数变化须重批；审批不等于 OS 沙箱）。消费已入库基线 `40da490`：Engine 工具循环、schema 6 持久化、`approval.requested`/`approval.resolved` 协议预留事件 kind、plugin_host 的 builtin 生命周期。收到用户交付本有效包后，B 登记来源/接手时间/执行标识即可工作，不重复要求形式确认。

## 1. 交付能力与边界

实现 Host 应用层的 ToolCall 逐次审批：模型发起的副作用工具调用（写文件、执行命令）在 `workspace::execute` 之前经 capability 分类与审批闸门 → 生成持久化 pending 审批（schema 7 `approvals` 表）→ 发出 `approval.requested` 事件 → 任务内等待决定 → 批准后按原路径执行 / 拒绝后给模型一条不含秘密的 tool 错误结果 → `approval.resolved` 事件。审批绑定单次 `tool_call` 的 id 与参数摘要，是一次性决定，不是可复用授权。pending 跨重启不丢：重启后 `decide_approval` 仍可对旧 pending 生效，`resume` 按审批行 reconcile 未应答 `tool_call`，不再把它们一刀切封口为"已中断"。

依据：项目书 §3.4（读取默认允许；写入、命令、额外网络、Git 合并逐次审批；插件权限声明不等于自动授权，实际调用经过 Host capability policy 与审批状态）、§5 验收（写入、命令和网络都经过审批；停止、失败和重启不会丢失或伪造完成）。`run_command` 是当前网络/Git 副作用的唯一载体，因此一律进入审批。

明确不做：

- 审批决定的 HTTP/CLI/UI 入口：`Engine::decide_approval` 是库级 API，传输面是后续切片（C）；
- PluginHost/插件化 capability provider：契约只在分类/评估缝预留接入点，本轮不把 `plugin_host` 挂进进程；
- 自动放行规则、持久授权、"总是允许"类策略；v1 策略只有静态分类；
- `run_wasm` 审批：当前 wasm 工具无宿主能力（无 host import），按直通处理并记录此取舍；
- 审批超时/过期重批；等待期间释放 `slots`/`key_slots` 并发许可（接受并记录占用限制）；
- 新增网络/Git/MCP 专用工具、新 UI、真实凭据、付费 live；
- 改变 `Task.status` 自由字符串语义或引入 running 内子状态；不使用 `AwaitingApproval` 表示"运行中等待工具审批"（协议该态是派工前状态，`Running` 不可转入）。

## 2. 基线、Git与唯一所有权

| 项目 | 约定 |
| --- | --- |
| Git 模式 | **独立工作树**（[Git 协作规范](../../../../Git协作规范.md) §2/§3） |
| 基线完整 SHA | `40da4903ab603d805bc1671a148b39aa86b0b7fc` |
| 分支 / 工作树 | `codex/b/approval` / `../nhc-b-approval/`（仓库根旁的同级目录；经理已创建并核对 HEAD=基线） |
| 提交执行人 | 员工B在任务分支提交本人范围内的小步修改；共享树 `./` 仍由经理唯一 Git 写入 |
| 整合人 | 经理：审查候选 SHA 后按 §6 串行整合到 `main` |
| 报告与身份 | 写共享树 `./niuma/员工B/…`；不编辑工作树内的 niuma 副本，工作树内只提交授权源码 |

唯一源码写入人为B，限定以下六处（相对工作树 `NoManCode/rust-app/`）：

1. 新增 `src/approval.rs`：capability 分类、策略评估、审批记录类型、preview/摘要与脱敏、typed 错误，必要内部单测。
2. `src/repository.rs`：schema 7 迁移（`approvals` 表）+ 审批 CRUD + `verify_current_schema` 扩展；沿用现有迁移标记/校验惯例。
3. `src/store.rs`：审批 API（建卡/决定/查询/pending 列表）、`approval.requested`/`approval.resolved` 事件发出、`recover` 对 pending 审批的语义说明与实现。
4. `src/engine.rs`：`run_task` 闸门、pending 等待与唤醒、`decide_approval` 入口、`resume`/`cancel` 的审批 reconcile。
5. `src/lib.rs`：仅追加 `pub mod approval;` 一行，不改其他导出。
6. 新增 `tests/approval_gate.rs`：公开 API 集成测试。

B 可更新自己在共享树的第二轮报告、证据及个人身份。任务与review由经理维护。不得修改 `domain.rs`（审批类型集中放 `approval.rs`；`approval.requested`/`approval.resolved` 事件 kind 已在 `validate_event_kind` 预留）、`server.rs`/`turn_http.rs`（无传输面）、`workspace.rs`（闸门在 Engine 层，execute/resolve 保持第二道防线）、`plugin_catalog.rs`/`plugin_host.rs`、`secrets.rs`/`provider.rs`、crates/、Cargo.toml/Cargo.lock、脚本、既有测试、web/ 及他人 niuma 档案。std + 现有 `anyhow`/`serde`/`serde_json`/`tokio`/`rusqlite`/`sha2`/`uuid` 足够，不新增依赖。需要拆文件、改既有签名或新依赖先由经理修订。

## 3. 数据契约

语义固定，Rust 签名可选借用/owned 以保持实现清晰，但须提供下列可被集成测试调用的公开入口。

### 3.1 Capability 分类与策略评估

- `approval::classify(tool_name) -> ToolEffect` 纯函数：`list_files`/`read_file`/`search_files`/`run_wasm` → `ToolEffect::None`（直通）；`write_file` → `ToolEffect::WriteFs`；`run_command` → `ToolEffect::ExecCommand`（网络/Git 副作用经 shell 发生，归入命令类）；其他 → `ToolEffect::Unknown`。
- `approval::evaluate(task, tool_name) -> PolicyDecision`：`Allow` | `RequireApproval` | `Deny{reason}`。v1 规则：`ToolEffect::None` → `Allow`；`WriteFs`/`ExecCommand` → `RequireApproval`；`Unknown` → `Deny`（现有 definitions 白名单已先拦，不新增可达路径）。`evaluate` 是纯函数：不读文件/环境/网络/时钟；后续插件化 policy 在同一接缝替换实现。
- 分类表集中在一处且可单测；新增工具类型时的默认归类是"需要审批"而非"直通"，文档写明该默认。

### 3.2 `approvals` 表（schema 7）

`SCHEMA_VERSION` 升到 7，新迁移标记按现有惯例登记；`verify_current_schema` 扩展为 schema 6 段 + schema 7 段，schema 6 既有校验不得放宽。列（类型与现有表同风格）：

| 列 | 约束与语义 |
| --- | --- |
| `id` | TEXT PK，与任务/回合 id 同生成方式 |
| `tool_call_id` | TEXT NOT NULL UNIQUE；绑定模型单次调用的 `call["id"]`，审批不可转用于其他调用 |
| `task_id` | TEXT NOT NULL；`turn_id`/`session_id` TEXT NULL（legacy run 可无） |
| `tool_name` | TEXT NOT NULL |
| `args_digest` | TEXT NOT NULL；规范化 JSON 参数的 sha256 hex，绑定参数快照作审计证据 |
| `preview` | TEXT NOT NULL；脱敏摘要，见 3.5 |
| `status` | TEXT NOT NULL，`pending`/`approved`/`denied`/`cancelled` |
| `created_at` | 与现有时间列同型 |
| `decided_at`/`decided_by` | 可空；`decided_by` 例 `user`、`system:task-cancelled` |

索引 `(task_id, status)`；`tool_call_id` 唯一约束。已迁到 schema 6 的库打开时自动补表；DDL 中途崩溃可重入（参照 `store.rs` 现有迁移测试惯例）。新表 CRUD 走事务，与事件写入同一事务边界，不出现"行已建而事件丢失"或反之的持久状态。

### 3.3 Engine 闸门（`run_task`）

插入点：definitions 白名单检查（`engine.rs` 现约 628–631 行 `ensure!`）之后、`file_backup` 与 `workspace::execute` 之前。对每个 `tool_call`：

- `Allow` → 现有路径不变（backup→execute→tool_result）。
- `RequireApproval` → 若该 `tool_call_id` 已有审批行则复用（不重复建卡、不重复发 `approval.requested`），否则建 pending 行 + 发 `approval.requested` → 注册一次性等待通道 → `select!` 等待【决定到达】或【任务取消】：
  - `approved` → 走原 backup→execute→tool_result 路径；
  - `denied` → 写 `approval.resolved` + `role=tool` 错误结果（文本不含秘密、不含原始参数），不调 `execute`，循环继续；
  - 任务取消 → 该任务全部 pending 审批置 `cancelled`（`decided_by=system:task-cancelled`）+ `approval.resolved` 事件 + 唤醒等待方，任务走既有取消路径。
- 等待期间持有 `slots`/`key_slots`（本切片接受该占用并写入已知限制）；审批等待不做超时。
- 拒绝不冒充成功：未获 `approved` 不得调用 `workspace::execute`，也不得写成功形态 `tool_result`。

`Engine::decide_approval(approval_id, approved, decided_by) -> Result<ApprovalRecord, _>`：行不存在 → `ApprovalNotFound`；行非 pending → `ApprovalConflict`（携带当前 status）；通过则同事务置 `status`/`decided_at`/`decided_by` + 发 `approval.resolved`；存在在程等待者则 send 唤醒。无等待者（重启后）仅持久化，执行在 `resume` reconcile 时发生。`decided_by` 由调用方给（库级 API 默认 `user`）。

### 3.4 重启与 `resume` 语义

- `Store::recover()`：`queued|running → interrupted` 不变；`approvals` 的 pending 行**保持 pending**——审批是用户决定队列，不随进程消亡自动失效或自动批准。
- `resume()` 对未应答 `tool_call` 的 reconcile 改为：按消息顺序逐个检查审批行——
  - 无审批行 → 维持现 `"Execution was interrupted…"` 封口；
  - `pending` → 不封口；留在任务中，由 `run_task` 重启后按 §3.3 闸门重新等待决定；
  - `approved` → 不封口；`run_task` 启动后按原路径执行该工具并写结果；
  - `denied`/`cancelled` → 写对应 `role=tool` 错误结果（不含秘密）。
- 已 reconcile 的调用全部有结果后，才携带新用户消息进入正常 provider 循环；工具结果顺序必须与 `tool_calls` 顺序一致。
- 决定已下但任务不再 `resume` 时不执行任何工具：审批批准只放行，不主动产生副作用。

### 3.5 事件与脱敏

- `approval.requested` 数据：`approval_id`/`tool_call_id`/`tool_name`/`preview`/`task_id`/`turn_id`/`session_id`；`approval.resolved` 数据：`approval_id`/`status`/`decided_by`/`decided_at`。事件写入与审批行写同事务，过 `redact_persisted`。
- `preview`：`write_file` → 规范化相对路径 + 字节数（不含文件正文）；`run_command` → 命令字符串经 `secrets::scrub` 后截断 ≤160 字符。一律不含完整 `arguments`、环境变量、Key、文件内容。
- `args_digest` 对 `serde_json` 规范化序列化取 sha256；同一调用重放得到同一 digest。

### 3.6 错误与确定性

- typed 错误稳定变体至少含 `ApprovalNotFound{id}`、`ApprovalConflict{id,status}`；不 echo 原始参数、路径正文或秘密。
- pending/审批列表查询按 `(created_at, id)` 确定序返回；内部遍历用 BTreeMap/排序，相同输入产生相同顺序。
- 闸门与 reconcile 不引入时钟/随机数依赖（id 生成沿用现有方式除外）。

## 4. 必须验证的场景

每项报告给测试名、断言、实际结果；测试在 `tests/approval_gate.rs` 使用公开 API，夹具沿用 `runtime.rs`/`session_turns.rs` 的 `Store::open`+`Engine::new` 模式，mock provider 能按脚本发出 tool_calls。

| 编号 | 反例或场景 | 必须断言 |
| --- | --- | --- |
| AP01 建卡等待 | mock 发 `write_file` 调用，不决定 | `approvals` 行 pending、`approval.requested` 事件含 preview；`workspace::execute` 未发生（目标文件无变化）；无 `tool_result` |
| AP02 批准执行 | `decide_approval(approve)` | 文件被写一次、`tool_result` 成功、`approval.resolved`、行转 approved；`decided_by`/`decided_at` 记录 |
| AP03 拒绝续聊 | `decide_approval(deny)` | 未执行（文件不变）；`role=tool` 错误结果不含秘密与原始参数；`approval.resolved`；任务随后正常完成或继续循环 |
| AP04 读取直通 | `list_files`/`read_file`/`search_files` 调用 | 不产生审批行、无 requested 事件；正常 `tool_result` |
| AP05 命令审批 | mock 发 `run_command` | 同 AP01–AP03 闸门语义；preview 为脱敏命令摘要 |
| AP06 重启待批 | pending 时 drop Engine/`Store::recover` 重开 | 行仍 pending；`decide_approval(approve)` 后 `resume`：该 `tool_call` 不被 interrupted 封口，工具真实执行一次，任务完成 |
| AP07 重启拒绝 | 同上但 deny | `resume` 产生 denied `tool_result`（不执行）；任务可继续/完成 |
| AP08 取消 | 等待中 `Engine::cancel` | 审批行 `cancelled`+`approval.resolved`；等待方被唤醒；无 `execute`；任务终态 cancelled |
| AP09 决定边界 | 未知 id、重复 decide、对 denied/cancelled 再 decide | `ApprovalNotFound`/`ApprovalConflict` typed 错误；行与事件不重复写 |
| AP10 脱敏 | preview/事件/行内容扫描 | 不含 mock key 字符串、文件正文、完整 arguments、env 项；`args_digest` 对同参稳定、异参不同 |
| AP11 多次审批 | 同任务两次 `write_file`（不同 `tool_call_id`） | 两行独立审批；批准第一个拒绝第二个 → 只有第一个文件变化；顺序与 `tool_calls` 一致 |
| AP12 回归与迁移 | schema 6 库升级、既有测试 | 预置 schema 6 库打开后补表且 verify 过、行数据保留；`cargo test --workspace --locked` 全绿；`git diff 40da490` 只含授权六处 |

## 5. 验证与交付

独立工作树即隔离环境：在 `../nhc-b-approval/NoManCode/rust-app` 直接跑门禁，工作树内 `target/` 被忽略，首次构建为全新编译。报告/身份写共享树 `./niuma/员工B/…`；共享树 `./` 的暂存与提交仍只有经理执行，B 在工作树分支自行提交授权源码的小步修改。

环境沿用 [联合门禁记录](../../../项目经理/审查记录/2026-09-23C-D联合门禁.md) 与 D 证据 `msvc-env.ps1` 记录的本机事实：pwsh 为 Git 内嵌 shim（PS 5.1）、VS 非标准位置需显式注入 `VCToolsInstallDir`/`VSINSTALLDIR`/`WindowsSdkDir`、Git 目录前置 PATH；Git Bash 直跑 cargo 需 `MSYS2_ENV_CONV_EXCL='LIB;INCLUDE'`。同一 pwsh 进程先 `& ./build.ps1 -Action check` 建环境（记录退出码），再运行：

```powershell
cargo test -p peachsh --locked --test approval_gate
cargo test -p peachsh --locked --lib approval::
cargo clippy -p peachsh --lib --test approval_gate --locked -- -D warnings
```

交付前在工作树对当前分支候选完整执行 `build.ps1 -Action test`、`-Action fmt`、`-Action clippy`、`-Action wasm-check`（fmt 仅检查）；不执行 build（复制 EXE）、不执行 live（付费）。`git diff 40da490` 只含授权六处；共享树另查报告/身份文件。

报告：`niuma/员工B/提交报告/第二轮/员工B（ToolCall审批与Capability网关 第1.0轮报告）.md`；有限脱敏附件同目录 `B-R2-01证据/`（相对路径，不写本机绝对路径）。经理review目标：`niuma/员工B/审查记录/第二轮/员工B review 1.0.md`，当前不存在即未审查。报告记录实际执行模型、任务执行标识、分支 `codex/b/approval` 与候选提交完整 SHA、AP01～AP12 映射、失败/未执行项、范围与后续限制；更新本人身份，交回报告路径。报告提交、Git 提交、验收、合并、发布各自记录；员工在工作树分支的提交不等于经理验收或整合。
