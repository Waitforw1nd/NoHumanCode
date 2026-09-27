# NEXT-13 CLI 与 HTTP 只读建议

2026-09-27，员工C/GPT-6-astra。新任务 C-R14-PRE01，执行ID C-R14-PRE01-20260927-E01；不恢复已结项C-R13。真实核收main `b8fa6dd19229a97c80b0657694b77308b7109993` / NoManCode `191687949486545e22ed0769eaa32aee5d6faf6a` / Rust `ae885d552659810753d00ff796dd542249d21197`。接收时仅经理管理文档dirty及前置登记未跟踪，产品无修改。只读源码/契约，无产品修改、测试、建树或Git写入。本报告是建议，所有新增接口名称和字段均待经理冻结，不是已有能力或实现派发。

## 推荐边界

第一片采用“用户显式创建linked worktree，成功后登记为独立Project”。shared继续引用已登记Project的原根；isolated引用新Project的worktree根。两者最终都走现有显式project_id + expected_config_revision的Run入口。不要在首次Run时隐式建树，也不要复用同Project让不同Session改变根。

依据：src/project_config.rs 的 TaskProjectBinding绑定project_id/root_identity/config_revision/permission_digest，validate_task_tx验证Task→Session→Project链；network_authorization.rs的route subject带project_id。独立Project可复用当前恢复/审批/授权边界。同Project per-Session根需要新增workspace身份并贯穿Task、Turn、审批、网络grant、checkpoint/restore、Run context，第一片代价明显更大。linked worktree拥有独立工作目录/index，仍共享Git refs/objects，不承诺仓库完全隔离或OS沙箱。

“shared/worktree”应是可查询的持久意图与实际关系，而非只在CLI加标签。普通新Project可标识shared；既有Project不伪造为managed worktree。worktree Project记录source_project、原始请求base、冻结resolved_commit、分支/目标目录和关联operation_id，实际Git身份由Host验证。不能把目录复制登记成Git隔离。

## 最小用户闭环

1. shared：用户查询 `project list/get/config get`，确认根与配置；新流程明确选择已有Project，调用已有 `run start --project P --expected-config-revision R ... --key K`。无需切换全局project-selection；该全局选择不能改变已存在Session。旧不带project的Run继续沿现有Host默认策略，不静默新增隔离行为。
2. isolated：建议新增独立的Host创建操作，例如 `project worktree create --source-project P --expected-config-revision R --base REF --branch NAME --path PATH --name NAME --key K`；明确说明只基于冻结commit，源工作树未提交内容不自动复制。首片选择“新分支、全新目标目录”，避免顺带支持已有分支复用/已有目录接管。基线移动、目录占用或同分支并发均报冲突，不暗中换名、覆盖或加force。
3. create回执提供operation_id、意图、实际状态和成功后的project_id；只有durable succeeded且身份验证一致才能作为Run目标。不同时启动Provider/Agent，不自动切当前Project。
4. 新Project默认最小权限，沿现有config get/set显式设置允许route、tools、write_scopes、allow_commands（CAS revision）。即使引用同一个Host route/凭证，也不能复制旧Project grant；必须对新project执行network preview再authorize，使用新preview返回的revision/credential revision/binding digest/purposes。没有grant不得Provider调用。shell仍每次审批和network acknowledgement，目录隔离不等于shell沙箱。
5. 用新Project明确启动Run；用run context/task project核对根与绑定；新Session Turn仍固定该Project。创建worktree失败或未知时不得启动Run、不得退回source Project。

config文件读取是CLI输入读取，允许；创建目录、执行git、打开本地DB、写Project记录都必须由Host执行。CLI只bootstrap/auth HTTP/校验回执/展示结果，不实现“Host不可用就本地git”的旁路。

## 建议HTTP契约与安全DTO

建议新路由 `POST /api/projects/{source}/worktrees` 和认证只读的operation查询入口，与旧 `POST /api/projects` 登记现有根分开。旧RegisterProjectBody只有name/root_path且deny_unknown_fields；不要给旧DTO静默塞mode后兼容性含糊。新POST要求Idempotency-Key，明确source ID来自路径；请求DTO精确字段、禁止未知/重复字段、限制body/字符串大小和数值范围。服务端传参用argv，不接受raw git命令、shell片段或任意git选项。

回执建议版本化对象：operation_id、source_project_id、request_digest、state、resolved_commit、target_path、branch、project_id（仅成功）、created_at/updated_at、安全error code、retryable=false。禁止仅靠HTTP2xx推断成功，校验state与project_id互斥关系、requested source/参数和回执一致、合法ID/hash、路径安全字符串。公开诊断不返回token/密文/git环境/完整stderr；绝对目标路径是产品状态可展示值，不应混同报告路径可移植限制。

新路由要接入server::guard与CLI authenticated_read。已有guard覆盖/api/projects前缀，若operation入口另起/api/worktree-operations则须显式纳入认证读；所有写仍要求token，Host/Origin/sec-fetch-site校验及安全响应头保持。不要让状态查询暴露本机目录而绕认证。

保留旧Project/Run/Session/错误DTO语义。CLI current workflow外层只把Run/Session/Task若干写操作的invalid/unprintable回复转为unknown；新增worktree写操作必须显式加入该防线，包括传输中断、截断/畸形/字段不一致响应、输出失败，不能把“回执校验失败”误表述为“创建未发生”。这是新增实现注意点，不是本轮已修缺陷。

## claim、unknown与查询恢复

必须先持久化意图/唯一key/冻结base及目标，再取得durable claim才允许git副作用；数据库事务与Git不是原子事务。created/requested、claimed、succeeded、failed、unknown状态转换及重启策略由B/D共同冻结。只有可证明尚未产生副作用的拒绝能称failed；进程已spawn但退出/写盘不确定，或Git完成但登记Project前崩溃，都保持unknown，不能只看exit0伪称整体成功。

至少提供按operation_id查询；响应丢失导致客户端不知道ID时，还需认证按key查回执/列出source下最近操作的明确路径。推荐显式 `status --key K --source-project P`（key不放容易记录的URL query，可设计认证只读查询POST）；契约也可选择手动原POST同key返回既有记录，但必须严格保证claimed/unknown不重新spawn。重放同key不同参数稳定冲突，冻结resolved_commit不因符号REF后来移动改变。

status只读，unknown不能自动retry、删除目标、unregister、prune或创建另一目录。最小闭环允许unknown终止并给出可操作诊断：source身份、目标存在性、branch/HEAD/worktree登记匹配结果、Project是否已绑定及错误码；不可由CLI直接修复。若第一片不做人工确认完成API，要明确“只诊断、保持unknown，需后续授权修复”，不要把它说成自动恢复完成。可证明成功的只读观察也不应无单独契约就悄悄修改持久状态。

## 冲突与路径拒绝

建议稳定分类：invalid（400）、not_found（404）、revision/idempotency/branch/target/root_changed/source_busy冲突（409）、git_unavailable/unavailable（503）、internal/corrupt（500）；产品完成未知可按契约409或202表达，但DTO必须同样可机器判断、retryable=false，不能依赖HTTP单一状态。与project_*现有错误code映射保持兼容，新码需显式加入CLI allowlist。

Host验证source Project当前根身份、实际Git根/common-dir关系、目标父目录身份、目标不存在且与所有Project根不重叠、目标不在source/.git或其祖先/子树、reparse/junction/symlink及大小写路径别名、保留设备名/ADS/路径穿越/非法branch或REF。目标目录尚未存在不能只canonicalize目标字符串；应验证父链并在副作用前后复核实际身份。源码当前RootIdentity是复核观察，不是handle沙箱，路径并发替换的残余边界须如实说明。产品不可硬编码E盘；E只是本次工程操作约束。

分支/目录冲突不force、不reset/clean/stash/stage/commit/merge/push；新branch创建是本操作明确授权的Git写入，其他Git行为不连带授权。是否拒绝source有活动Agent/Run，以及跨两个Host同时建同branch/target的串行机制，需经理与B/D冻结，不能仅靠单进程mutex宣称互斥。

## 后续正式验收建议（本轮均未执行）

- 真HTTP+CLI正控：shared显式Run仍在原根；worktree成功得到独立Project，在新根做一次审批写入，source工作文件/index未改；实际git identity/branch基线可核对；创建自身Provider调用0。
- 权限正反控：新Project默认无授权；即使source已grant，新ProjectModels/Completion仍拒绝且Provider计数不增；新preview/authorize后新Run正控。shell审批/ack和files scopes保持，旧Session不能被select或模式切换改根。
- 安全DTO：Host/Origin/token、重复/未知字段、过大请求、错误revision、跨source key复用、伪ID/不一致回执、截断响应/输出失败；CLI不本地碰DB/FS/Git，不泄漏token/命令stderr。
- 真Host子进程持久停点：durable claim后spawn前kill+wait；Git副作用后Project提交前kill+wait；提交后响应发送前kill+wait。每例重启计数无新增spawn/Provider、状态/Project/Git登记/目标目录一致、原key不重做、unknown不自动清理。无响应丢失不能用普通happy path代替。
- 路径/并发：同目标/同branch/同key与异参两个Host竞争，路径别名/reparse切换/现有未登记目标、source移动、branch checked-out冲突。逐项验证失败后不误删用户目录或改变旧Project，失败与unknown区分。
- 旧协议回归：旧project create/config/select、默认/显式Run、session send、恢复封存、network grants、run context及严格CLI拒绝混搭保持；最终固定组合后再按经理授权五门禁。

## 待经理冻结项

优先冻结独立Project方案、第一片新branch/新目录限制、创建操作状态机及按key查询能力、unknown是否仅诊断、source忙态规则和跨Host互斥、Git默认配置/钩子/过滤器副作用边界、路径所有权证明与错误码。确认后再派实现与唯一文件所有权。本方只读建议完成；下一第一操作等经理范围冻结或有效补充任务，不自行实现/测试。
