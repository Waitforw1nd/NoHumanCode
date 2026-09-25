# C-R3-01 / NEXT-02D：审批HTTP正式契约（修订1）

2026-09-26；制定人：主对话项目经理GPT-6。用户“下一步”承接既有多GPT-5.6-sol员工授权，经理直接派发。本契约取代未派发草案的实施约定，草案保留历史。B-R2修订2已验收整合，真实API冻结见[B review 1.1](<../../../员工B/审查记录/第二轮/员工B review 1.1.md>)。

## 1. 所有权与交付

| 项目 | 有效约定 |
| --- | --- |
| 唯一实现员工 | C，实际模型gpt-5.6-sol；登记真实接收时间及执行标识，一任务一个执行者 |
| 完整基线 | `379e2aa6378bbe1161c9f65f6655f93876834ac0`（已含NEXT-02C整合与档案） |
| 独立工作树/分支 | `../nhc-c-approval-http/` / `codex/c/approval-http`，经理已创建 |
| 唯一源码范围 | `NoManCode/rust-app/src/server.rs` 和新增 `NoManCode/rust-app/tests/approval_http.rs` |
| 提交/整合 | C仅提交本人两处；经理review后串行整合。共享index仅经理操作 |
| 档案 | 共享仓库niuma/员工C，禁止写工作树niuma副本；第1.0报告新增 `提交报告/第三轮/员工C（审批HTTP接入 第1.0轮报告）.md`，已有则增1.1 |
| 其他员工 | B只读真实接口预检，A只读安全契约/候选复核；无产品写权、不代改C两处 |

不修改Engine/Store/Repository/approval/domain/secrets/workspace/protocol/Cargo/旧测试/UI/CLI；无新增依赖。必要跨层支持先报具体事实给经理，不自行扩大。保留共享历史及旧工作树，不reset/clean/stash覆盖。

## 2. 三条路由与安全DTO

| 路由 | 200响应 |
| --- | --- |
| GET `/api/tasks/{id}/approvals` | `{"approvals":[DTO...]}`，按Store `(created_at,id)`顺序；存在task无记录空数组，未知task404 |
| GET `/api/approvals/{id}` | DTO本身，无额外包装 |
| POST `/api/approvals/{id}/decision` | DTO本身；body仅 `{"decision":"approve"}` 或 `{"decision":"deny"}` |

路径id用现有PathRejection结构化处理后，通过既有validate_persisted_id检查，错误固定安全400，不回显id。body使用deny_unknown_fields、严格枚举；空/多余/重复字段、错类型/null/非法枚举/语法错误400；ContractJson沿现有415/413约定。decided_by固定user，不收客户端身份；不要求或新增Idempotency-Key（重复决定409），不为客户端额外字段开后门。

显式白名单DTO，不能直接序列化ApprovalRecord。字段：

```text
id, task_id, tool_call_id, tool_name, preview: String
session_id, turn_id, decided_by: Option<String> （null保留）
status: ApprovalStatus (pending/approved/denied/cancelled)
execution_state: ExecutionState (not_started/claimed/finished/unknown/cancelled)
created_at: u64
decided_at: Option<u64>
```

不返回args_digest、binding_digest、workspace、write_scopes、allow_commands、原参数、SQL或任务完整内容。preview只取已脱敏应用摘要，不重新展开参数。GET unknown/finished均返回实际状态200，不从批准决定推导执行成功。POST DTO反映决定事务提交时快照；活跃worker可能紧接着claim，允许响应not_started与后续GET状态不同，不要求等待副作用完成后才响应。

沿现有Host/Origin/sec-fetch-site guard；POST须现有x-peachsh-token，GET安全策略保持现有本机读规则。保持1MiB body limit、三安全头、无CORS放宽；新接口所有错误使用 `code/message/retryable/error` JSON，error等于message，固定安全文案。不要为本轮修改全局旧接口错误行为。

## 3. 真实应用入口与错误映射

依赖真实公开接口（Result为anyhow::Result）：

```rust
Engine::decide_approval(&self, approval_id: &str, approved: bool,
    decided_by: Option<&str>) -> Result<approval::ApprovalRecord>;
Store::approval(&self, id: &str) -> Result<approval::ApprovalRecord>;
Store::approvals_for_task(&self, task_id: &str) -> Result<Vec<approval::ApprovalRecord>>;
Store::task(&self, id: &str) -> Result<Task>;
```

列表先Store::task确认存在（真实QueryReturnedNoRows才能404；JSON/身份损坏500），再查审批列表。单卡/决定按ApprovalError分类：NotFound404，Conflict/BindingConflict/UnknownResult409，CorruptState500。沿error chain类型判断，损坏不能因底层缺关系行而变404；其余存储/内部错误500，所有HTTP输出为安全固定文案。只有明确保留的SQLite DatabaseBusy/DatabaseLocked链才retryable=true；Conflict及unknown均false。不以字符串解析中文错误，不走通用anyhow→400。

重要草案纠正：目前decide仅CAS pending→approved/denied，并不执行绑定重查；BindingConflict/UnknownResult在claim/resume路径产生。HTTP handler不得自行复制绑定算法/SQL、调用ensure/claim/finish/resume/execute/provider。对已unknown（其决定通常approved）重新decision会得到Conflict409，不伪称发生新一次未知执行。pending参数被篡改时，决定可能200；之后Engine执行前必须以既有BindingConflict拒绝，零执行。这是已冻结应用语义，本轮不承诺decision本身原子校验绑定。

另一个已知应用限制：部分Repository查询把SQLite错误转换成CorruptState并丢失底层cause；该场景必须安全500、retryable=false，不能猜busy。写决定事务开始处保留的busy/locked可500/retryable=true。本轮不擅改B层；报告清楚区分，不能拿假mapper探针宣称所有GET锁故障均可重试。

决定由Engine事务提交后通知，活跃等待者获批准可以合法继续执行；无等待者只持久化，不能启动worker。取消复用既有Engine/HTTP入口，claim先于取消无法撤销外部副作用。unknown不重试、finished不重做；此传输切片不新增自动恢复API。

原Run/Session SSE透传 `approval.requested` / `approval.resolved`，保持原envelope、task/turn/session归属、seq/cursor和断线语义，不另建SSE。需扫描新DTO/审批事件无正文/参数；既有task消息本身会保存经过适用脱敏的普通参数、file_backup可能保留旧内容，不将“整个数据库/SSE所有事件无任何正文”写成不可能契约。可识别mock凭据不得新增泄露。

## 4. AH01～AH14 验收矩阵

真实HTTP端口、本地mock provider、临时数据库/工作区；以HTTP响应+持久行事件+副作用共同断言。测试可直接Store准备数据/注入单故障，但新三路由必须从HTTP验证，不能只调用handler。错误分类不可达分支可以server内单元测试，报告明确非真实HTTP触发。

| 编号 | 必须证据 |
| --- | --- |
| AH01 | 真实write_file pending，GET列表/单卡准确安全DTO且确定序；目标无变化、无backup/start/result；存在任务空列表和未知任务404 |
| AH02 | HTTP approve真实文件仅一次；HTTP deny无写、安全tool错误续聊，resolved一次；200快照与后续GET执行状态分别断言 |
| AH03 | 读取工具直通，无审批行/requested，真实结果 |
| AH04 | 真实PowerShell计数命令，批准前0/后1、deny0；不把spawn失败当拒绝 |
| AH05 | 真shutdown旧runtime后重开、recover保留pending；HTTP决定时无worker且provider/文件无增量，再显式resume执行一次；drop Arc不合格 |
| AH06 | denied重启不执行；真实finished重开不重做；真实claim+命令副作用后停止runtime恢复unknown，GET200实际状态、重新决定409且无再次副作用，禁止手造finished替代真实执行 |
| AH07 | pending公开取消与HTTP决定的同步竞争，只一次resolved；确定性取消先于claim场景零执行；claim先不抹已发生事实 |
| AH08 | 屏障并发approve/deny及重复/反向决定，恰一200其余409，行决定一次、resolved一次 |
| AH09 | DTO字段精确白名单，审批事件与响应不含mock凭据/正文/原参数；命令preview字符上限/脱敏与B契约一致，不夸大既有所有SSE事件内容限制 |
| AH10 | 两task合法同call id，经HTTP各自决定、不借权；列表隔离 |
| AH11 | malformed/重复字段/额外decided_by/错误类型/缺CT/超大body/path与guard/token拒绝，结构化错误和安全头，行/事件/副作用零新增；未知404、坏JSON/关系损坏500；保留SQLite链的真实busy决定故障500可重试、其他500不可重试 |
| AH12 | 无等待者HTTP批准只持久化，provider/工具次数不增；拒绝再决定409，不人为为请求launch |
| AH13 | Run及Session SSE归属/seq/requested/resolved，after或Last-Event-ID恢复不重复已消费seq，断开SSE后仍能审批执行；有限有界等待，不能sleep碰运气 |
| AH14 | GET unknown真实200，重复决定409不可重试无新resolved；pending绑定变更决定可能200但执行前fail closed（应用语义，不能伪称POST绑定409）。mapper对BindingConflict/UnknownResult分别有类型测试即可，不声称HTTP可达；报告分开 |

对并发/超时先证明前置条件，用屏障/通知/有界轮询，禁止以无关FK报错代替目标场景。AH06命令副作用窗口及AH05必须真正停止runtime/子进程，遵守旧AP安全边界。

## 5. 门禁、报告与交接

先识别真实PS/工具链，标准build.ps1分别check/test/fmt/clippy/wasm-check；定向 `cargo test -p peachsh --locked --test approval_http`。MSVC从本机现有安装进程导入，不修改脚本；不付费live/发布build。保存有限原始log、实际开始结束时间、退出码、固定SHA和源码清单；日志路径相对化需留说明，不伪造通过或未跑项目。

第1.0报告逐项AH证据、真实DTO/错误/路由、实际模型与执行ID、完整候选SHA、命令结果、限制和未完成项；更新共享C身份。提交源码前核对仅两文件、git diff-check、审staged diff，只提交任务分支。经理审查记录写C审查记录/第三轮，员工不得自行宣告验收/合并。

先完成实现和测试交候选；经理与A对固定SHA复核，必要具体返工。标准全部通过及阻断关闭后才串行整合，历史失败日志/草案/候选保留。
