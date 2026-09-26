# NEXT-06：真实 Git Diff 与单 Task 最小 Checkpoint

版本1.0，2026-09-27，经理GPT-6。用户已授权GPT-6-astra员工并行实现、调试、正式测试；经理负责契约/review/串行整合。本文件冻结后可直接派发，不要求用户重复形式确认。它不表示产品已经实现或通过。

## 基线与源码核对

- 新任务完整基线：`7a4a001d4296a26086d5196997ae80ac88e1c22b`（main）。NoManCode tree `ea07c43de8263161fcbeac1a3ab51e7b6b2b9e10`；Rust tree `ba569b4e5c4365d5e71a3075ecc7fafe48373bfa`。与NEXT-05受测候选的整个NoManCode差异为空。
- 303通过/0失败/1付费忽略及五门禁0是[NEXT-05既有证据](../审查记录/2026-09-27NEXT-05联合验收.md)，本轮未复跑。B-R4/C-R6/D-R3结项，当前代理清单只有经理；新任务不续用旧执行ID。
- `Engine::changes/latest_restore/restore`、Store workspace_changes/workspace_restores、Workspace安全路径/原子替换及Approval claim已核。changes只有摘要；restore按Task聚合最早before与最后after，先claim、逐路径outcome、恢复后封存。新功能必须复用这些不变量。
- 共享源码/index干净；九份继承档案不得暂存或覆盖，清单见[交接快照](../项目记录/2026-09-27整体项目状态与换对话交接.md)。所有旧工作树保留。报告/身份只写共享`./niuma`，不写员工工作树里的副本。

## 用户可见切片与明确边界

1. 通过CLI/HTTP查看当前Host所绑定Task工作区中一个明确相对路径的真实Git文本差异，分别比较HEAD→index、index→工作文件、HEAD→工作文件。不是changes清单，也不局限于工具产生的修改。
2. 对一个已completed且全部写入事实可信的Task，显式创建并持久保存“该Task首次写入前”的检查点；它引用已有受保护before镜像并冻结不可变manifest，恢复仍采用已验收的安全restore。显示`kind=task_before`，不能展示为创建时整个目录快照。
3. 最小检查点只覆盖该Task通过批准write_file修改/新建的普通文件字节与存在性。不会回滚run_command、用户其他文件、Git index/HEAD、ACL/ADS/时间戳；不执行git commit/reset/checkout/clean/stash。目录快照、任意时点/多Task检查点、重命名追踪和批量diff在后续切片另立契约。
4. 本片无模型可调用的新工具，没有新增自动授权。人通过有鉴权的CLI/HTTP显式请求创建/恢复检查点，沿用现有显式restore信任边界；模型不能把checkpoint查询当执行批准。

## A. 真实Git Diff协议（D实现，B接Engine，C接传输）

新增`src/git_diff.rs`：

```rust
pub enum GitDiffView { Staged, Unstaged, Head } // serde snake_case
pub struct GitDiffRequest { pub path: String, pub view: GitDiffView }
pub async fn read_git_diff(root: &std::path::Path, request: GitDiffRequest)
    -> Result<GitDiff, GitDiffError>;
// Engine（anyhow::Result可downcast业务错误）
pub async fn git_diff(&self, task_id: &str, request: GitDiffRequest) -> anyhow::Result<GitDiff>;
```

`GitDiff`公开字段冻结为`path, view, base_oid: Option<String>, index_oid: Option<String>, before_digest: Option<String>, after_digest: Option<String>, status, patch: Option<String>, redacted: bool`。`status`蛇形字符串/枚举为`text, unchanged, binary, too_large, missing`。base_oid为采样HEAD完整OID（unborn为null），index_oid为该路径index blob（缺失为null）；摘要针对原字节，不以脱敏字符串当恢复数据。patch只在text存在，是含文件头、hunk、增删行的真实unified diff；无变化明确unchanged，双方均不存在missing。允许直接安全Git diff或根据真实Git blob/工作文件生成准确unified patch；不能从changes元数据合成假正文。

- 请求必须指定单文件路径。没有任意ref/revision/pathspec/shell字符串输入；按`workspace::resolve(..., false, &[])`及safe_metadata_path拒绝穿越、绝对路径、`.git`、凭据路径、symlink/junction/reparse。Git pathspec采用literal，不让`:(...)`、通配符、前导`-`注入参数；带空格/中文文件名正常。
- 只接受工作区本身为Git top-level的普通仓库/合法linked worktree；不是Git、bare、上级仓库隐式发现或嵌套范围不一致返回Unavailable。不得读父项目内容。HEAD unborn支持空树基准；本片单路径重命名按删/增分别审阅，不声称识别rename。index unmerged、submodule/gitlink、Git symlink类型均Unsupported。
- staged显示HEAD→index，不掺未暂存字节；unstaged显示index→磁盘；head显示HEAD→磁盘。普通未跟踪文件在unstaged/head按空→文件，staged双方均无为missing；被ignore的未跟踪路径拒绝Unsupported，不能从ignore目录披露正文。删除须能显示真实删除patch。
- 每侧最多256KiB，二进制（NUL或非UTF8）/超限仅给状态、无正文；patch最多512KiB（序列化响应仍需满足CLI 1MiB上限，超限返回too_large而非截断伪装完整）。保留CRLF、无结尾换行语义。禁止无限收集stdout/stderr；每个Git子进程有界输出和<=10秒超时，终止并wait回收。
- 只读本地Git，禁外部diff/textconv、hooks、pager、smudge/filter与网络lazy fetch；清除继承的GIT_DIR/GIT_WORK_TREE/index/config注入等会重定向仓库或启动外部程序的环境，使用参数数组、不经shell。禁止任何Git写操作；Git stderr只进入安全分类，HTTP/日志无内部绝对路径/正文。按需读取对象缺失返回Unavailable，不fetch。
- 同一次查询前后复核HEAD、该路径index身份和相关工作字节；有变化返回Conflict，不能拼接不同时刻结果。Host gate阻止本Host并发副作用，外部变化仍需重核；不承诺抵抗恶意进程精确ABA或任意OS TOCTOU。
- 敏感路径先拒绝；正文经现有scrub识别的敏感内容不得进入响应/CLI/日志，改变时`redacted=true`，明确只供审阅、不可作为可应用patch。启发式不承诺识别任意未知秘密。本片不自动把diff放入Provider上下文、事件、数据库或报告。原始before/after正文仅内存处理。
- `GitDiffError`冻结为`InvalidPath, Unavailable, Unsupported, Conflict, Internal`；Engine额外按原WorkspaceChangeError报告Task NotFound/Active/当前workspace不匹配。各错误静态安全消息，不靠解析中文决定业务分支。

## B. Task-before Checkpoint（B唯一实现）

新增`src/checkpoint.rs`公开DTO；schema8→9原子迁移，由B独占repository/store。新增checkpoint表持久保存稳定UUID、task_id唯一外键、创建幂等key（受现有格式验证）、kind固定task_before、generation固定1、created_at、manifest_digest与不可变manifest。可独立子表或受严格验证的JSON manifest；schema校验必须检查列/唯一/FK/CHECK等实际约束，不能只认表名。旧业务/事件/审批/变更/恢复及其字节不得重写。

manifest冻结每个已成功变更的身份、task/root/scope/binding、路径键、before摘要和密文身份、after摘要及完整有序change成员。首次before和最后after决定恢复目标/当前期望；加密before继续保存在原受保护记录，创建时逐项解密核摘要，查询不披露正文/密文。不能只存一个changes列表再把任意后来的变化纳入恢复。

```rust
pub struct CheckpointEntry {
    pub path: String,
    pub before_digest: Option<String>,
    pub after_digest: String,
}
pub struct Checkpoint {
    pub checkpoint_id: String, pub task_id: String,
    pub kind: String, pub generation: u32, pub created_at: u64,
    pub manifest_digest: String, pub entries: Vec<CheckpointEntry>,
}
pub struct CheckpointCreated { pub checkpoint: Checkpoint, pub replayed: bool }
// Engine，内部共享同一个gate，不递归锁死
pub async fn create_checkpoint(&self, task_id: &str, key: &str) -> anyhow::Result<CheckpointCreated>;
pub fn checkpoint(&self, checkpoint_id: &str) -> anyhow::Result<Checkpoint>;
pub async fn restore_checkpoint(&self, checkpoint_id: &str) -> anyhow::Result<RestoreReceipt>;
```

- `CheckpointError`冻结为`Invalid, NotFound, Conflict, Unrestorable, Corrupt, Internal`。实际恢复错误继续使用`WorkspaceChangeError`及原RestoreReceipt，不制造另一套副作用成功事实。
- 首次创建需Task completed、当前Host workspace一致、同workspace无活动Task、非空可信finished变更、无prepared/unknown/未决审批/旧legacy不可信before/已有恢复；与所有Host写/start/resume/restore共用gate。在同一SQLite事务重核源记录与创建幂等；最多128规范路径、before总量<=8MiB，超过则Unrestorable、零checkpoint/事件写。所有当前文件需匹配最后after，否则Conflict，避免将外部编辑误标可恢复。
- 同Task仅一个generation=1检查点，同key回放原ID/manifest且replayed=true（即使随后恢复了也不创建新的）；不同key冲突。无自由名称、任意路径或客户端manifest输入。创建不触发模型、工具、Git或文件写；持久化成功后返回201，回放200；事务失败零新业务事实。
- Checkpoint身份永久绑定Task/workspace，不继承执行授权；新增checkpoint本身不封存Task，但后续旧Task写入须拒绝以保证manifest不可变（B同时协调prepare_workspace_change/resume）。下一次工作用同Session新Turn/new Task，检查点ID不同；不声称generation跨Task自增。
- 恢复：同gate下验证manifest与完整源记录尚一致，校验当前根/权限/活动/逐路径current摘要，再复用原restore路径。旧task restore与checkpoint restore必须收敛到同一个workspace_restore ID，不能双claim。直接task restore已complete时，checkpoint恢复返回原回执、不覆盖恢复后再次外改。任何持久manifest损坏failclosed；不能重建新manifest掩盖损坏。
- 保留预检失败零文件写、先持久claim、逐文件outcome、partial/unknown回执及重启unknown不重试。创建/查询/恢复事件只安全ID/计数/摘要/状态；密文和正文不进入DTO、日志或SSE。恢复后原Task永久写封存；complete后新Task承接，partial/unknown继续阻断新Turn，继承历史不继承批准。
- 新库、schema6/7/8→9连续迁移、重复open、故意伪造schema9必须验证。旧API保持兼容；不借新Checkpoint放宽原restore未知处理。

## C. HTTP与CLI（C唯一实现）

| API | 请求 | 响应/错误 |
| --- | --- | --- |
| POST `/api/tasks/{id}/git-diff` | `GitDiffRequest`严格JSON（只读POST，防止路径进入URL日志） | 200 `{"diff":GitDiff}`；InvalidPath400、Unavailable/Unsupported/Conflict409、Task404、内部500 |
| POST `/api/tasks/{id}/checkpoints` | `{}`严格JSON；必填单值Idempotency-Key | 201/200 `CheckpointCreated`；Invalid400、NotFound404、Conflict/Unrestorable409、Corrupt/Internal500 |
| GET `/api/checkpoints/{id}` | 无正文 | 200 `{"checkpoint":Checkpoint}`；NotFound404、Corrupt/Internal500 |
| POST `/api/checkpoints/{id}/restore` | `{}`严格JSON | 原restore包装`ok/restored/status/receipt`；complete200，partial/unknown409带安全receipt；错误映射不吞事实 |

全部沿用Host/Origin/token、persisted ID、Content-Type/body limit、安全响应头；未知字段拒绝。新错误固定code/message/retryable=false，未识别错误安全500。CLI命令：`task diff <id> --path <relative> --view staged|unstaged|head`（默认head）；`task checkpoint <id> --key <key>`；`checkpoint get <id>`；`checkpoint restore <id>`。JSON输出保持现有安全/有界传输惯例。差异text且redacted=false仍仅为审阅，不新增apply命令；binary/too_large等以JSON状态明确可见。恢复partial/unknown exit1且保留receipt；坏参数exit2；不自动重试POST、不换key，不允许重定向/代理泄漏token。只读Diff使用POST不表示有写授权。

## 验收矩阵

| ID | 必验事实 | 主要执行者 |
| --- | --- | --- |
| G01 | 临时真实Git仓库同文件staged与unstaged不同，三视图实际增删行准确，changes为空也可diff；无末尾换行/CRLF | D |
| G02 | unborn、未跟踪、忽略、删除、rename按删增、非Git/root不匹配、linked worktree、unmerged/submodule明确分类 | D |
| G03 | 中文空格/前导-及literal pathspec；穿越/敏感文件/实际链接拒绝，外部哨兵不读写 | D |
| G04 | 二进制/大文件/超输出/超时；外部diff/textconv/恶意Git env不执行；秘密scrub及安全错误；Git index/HEAD/工作文件查询前后不变 | D |
| G05 | 有确定性屏障的HEAD/index/工作文件读取期间改变→Conflict；不能仅靠偶发竞争 | D |
| K01 | 真实批准write_file的修改+新建+同路径两写→checkpoint→重开Store可查→restore最早before/存在性精确；不改变Git index/HEAD或其他文件 | B |
| K02 | 同key稳定回放、不同key冲突、并发创建唯一、失败事务零事件；创建后旧Task不能继续写，新Task独立ID | B |
| K03 | 外改/跨Task写/根或scope不符/活动/不可信legacy/unknown/空/超上限/解密失败/manifest篡改拒绝且零写 | B |
| K04 | task restore与checkpoint restore竞争同一claim/回执；多文件预检零写、执行中partial、重复complete不覆盖外改，真实gate屏障 | B |
| K05 | schema6/7/8→9完整保全、重复open、伪约束/篡改拒绝；敏感before不在DTO/事件/SSE | B+C |
| K06 | 检查点恢复claim前后及副作用后真正终止runtime/子进程，重启已claim→unknown、无自动重试、原Task封存/新Turn阻断；沿用原真崩溃夹具但必须证明新入口 | B+C |
| T01 | 真实HTTP/CLI diff→checkpoint create/get→restore→同Session新Turn；真Host kill/wait后检查点身份和回执保持 | C |
| T02 | 鉴权/Host/Origin/token、坏JSON/ID/key/未知字段、404/409/500、partial/unknown exit1且有receipt、请求不自动重复、秘密不泄漏 | C |
| T03 | 固定最终组合唯一员工C串行check/test/fmt/clippy/wasm-check；旧AP/AH/W/Turn/CLI回归不削弱，实际总数/失败/忽略如实记录 | C |

## 网络授权及后续顺序（本片冻结方向，不谎称已实现）

NEXT-06只运行本地只读Git，无fetch/网络工具。下一切片NEXT-07先落实网络权限：配置Provider目标origin和凭据引用由用户显式确认；开始/继续会话沿用绑定目标的模型请求授权，不给每个token/每次模型调用弹窗。路由/目标/凭据作用域变化须重新确认，redirect不得携密跨origin。显式HTTP/下载工具按一次调用的origin、method和用途/安全摘要审批，转跳越界重新授权；未知网络工具保持拒绝。

命令批准只证明用户批准那条命令；当前没有OS出站沙箱，不能承诺“批准本地命令就无网络”。NEXT-07需在审批展示并记录命令可能联网的事实，明确无网络保证时禁止作此承诺；不靠字符串扫描建立安全沙箱，不静默把allow_commands解释成任意网络批准。具体强制边界与旧审批兼容由NEXT-07单独契约/测试决定，本片不改既有命令行为。

NEXT-08随后做多项目配置/选择：稳定Project ID、canonical root、配置版本、路由/网络授权作用域绑定到Session/Task快照；切项目不复用旧批准，不改变已有Task根，不把一份全局settings改写当多项目隔离。配置→授权→Task三者的版本约束复用NEXT-07结果。之后再评估目录检查点/批量diff、Provider与持久调度缺口，阶段一验收前不扩Desktop/完整Team/MCP。

## 唯一所有权、工作树与派发

所有源码相对下列独立树的`NoManCode/rust-app/`。分支/目录由经理创建，员工是各自独立树唯一提交人；经理是唯一整合人/共享index操作者。共同完整基线为本文件顶部SHA；基线没有继承未提交产品源码。

| 任务/执行标识 | 工作树 / 分支 | 唯一产品写入范围 |
| --- | --- | --- |
| B-R5-01 / B-R5-01-20260927-N06 | `../nhc-b-checkpoint/` / `codex/b/checkpoint` | src/checkpoint.rs、engine.rs、store.rs、repository.rs、workspace_changes.rs（仅需要的typed兼容）；src/domain.rs仅checkpoint.created事件allowlist与必要验证；src/lib.rs负责checkpoint与git_diff两行导出；tests/checkpoint.rs；旧tests/approval_gate.rs、workspace_changes.rs仅schema预期/迁移适配；engine/store模块内相关测试 |
| D-R4-01 / D-R4-01-20260927-N06 | `../nhc-d-git-diff/` / `codex/d/git-diff` | 新src/git_diff.rs、tests/git_diff.rs；测试可用path模块引用，lib.rs由B导出，禁止D写lib.rs |
| C-R7-01 / C-R7-01-20260927-N06 | `../nhc-c-review-cli/` / `codex/c/review-cli` | src/server.rs、cli.rs、main.rs仅新CLI入口；新tests/review_cli.rs、checkpoint_http.rs；旧tests/http_contract.rs仅schema9预期；旧tests/cli.rs/cli_workflow.rs仅必要兼容及新入口测试 |

依赖计划：D独立实现真实Git读取并先交可审候选；B先提交DTO/迁移/API骨架固定候选（不得假实现成功），经理核查后串行合D→B及B接口→C。C可先写CLI参数/HTTP契约夹具，依赖未到不得自造B/D领域实现。B完成checkpoint/Engine后交固定SHA及定向证据；经理review并串行合到C最终组合。合并前对应员工明确干净冻结、无后台构建；员工不得自行合对方分支。

报告分别写共享`niuma/员工B/提交报告/第五轮/`、`员工C/提交报告/第七轮/`、`员工D/提交报告/第四轮/`；review写各自对应轮次。员工维护共享本人身份，提交完整SHA/源码tree、矩阵、失败/未跑、命令时间/退出码、原始与发布日志双hash。禁止release build、付费live、推送和旧树清理。模型服务失败保全原任务/在途文件，不静默换模或另开重复执行者。

正式派发与真实接收另登记[执行登记](2026-09-27NEXT-06执行登记.md)。A无本轮任务；经理不代写产品。任何新文件/跨模块改动先由经理修订唯一所有权，再执行。

### v1.0接入补充（2026-09-27）

B提出创建事件需要领域allowlist。经理核实际代码后明确事件名`checkpoint.created`，只由B在domain.rs的CORE列表及相关测试增加，已更新上表；payload仅checkpoint/task ID、计数、manifest摘要，创建事务内只一次，回放/GET不追加事件，恢复沿用原file_restore。manifest中的before密文可冻结其SHA-256而不复制；合法变化的restore_state/outcomes不参与不可变manifest比较，原source state/成员集合/根/scope/字节摘要仍必须一致。B先交不依赖git_diff模块的可编译Checkpoint固定候选，D合入后再加对应lib导出及Engine接入，禁止缺模块占位。
