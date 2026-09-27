# NEXT-13 Git工作树边界只读建议

2026-09-27，员工D GPT-6-astra；D-R11-PRE01-20260927-E01，新独立只读任务，D-R10不恢复。已读经理前置登记和Git协作规范。实核main b8fa6dd19229a97c80b0657694b77308b7109993；NoManCode191687949486545e22ed0769eaa32aee5d6faf6a/Rustae885d552659810753d00ff796dd542249d21197。开始status仅经理管理文档修改/新登记，允许且未触碰。Git模式固定基线只读，本轮无源码提交执行人、整合人仍经理；没有创建worktree、运行测试或任何Git写，仅本报告和本人身份。以下路径以仓库根为基准。

## 建议的第一片

采用用户显式创建linked worktree并登记为新的独立Project。shared模式沿用原Project；worktree是新Project而非偷偷改变旧Session/Task的workspace。新Project拥有自己的RootIdentity、配置、选择状态和授权流程，不迁移旧Task/Session/sidecar/checkpoint/grant，不自动启动任务。创建成功与选择Project分开，用户完成已有授权后才能新Run。目录隔离不是安全沙箱：linked worktree共享common dir、objects、refs及部分配置，shell可越出write_scopes，不能宣传两个Project互不影响仓库。

同Project内per-Session workspace需要绑定从project-root拓展为project+workspace实例，涉及Task根身份、sidecar、grant digest、恢复/checkpoint位置、活跃scope冲突和new Turn规则。当前project_config.rs:290/331依赖根身份封存，:406起登记根及重叠校验；直接给Task换路径会破坏已批准身份。本片不建议该策略，后续若做应独立迁移而非复用一个Project ID掩盖多根。

建议首片限制：已存在非bare、非unborn、本地完整对象的仓库根；只从观察到的HEAD commit创建新分支（detached HEAD可作为commit基线，unborn拒绝）。不接受任意revision表达式、不支持checkout既有分支、不自动fetch/submodule/LFS；新分支采用codex/workspace/<operation-id>或冻结的ASCII slug规则。是否允许自选已有本地branch/tag可以后续扩展；如首片必须支持，使用显式枚举类型LocalBranch/LocalTag/Commit，不接收任意Git rev语言。

## API与输入冻结建议

D新src/git_worktree.rs：inspect_repository(root_identity)->RepositoryObservation；prepare_worktree(source_identity,parent_identity,leaf,expected_head)->PreparedWorktree；execute_claimed_worktree(ClaimedWorktreePlan)->CreateOutcome；diagnose_worktree(operation_snapshot)->WorktreeObservation。Prepared不是权限、Claimed必须由B持久层创建且字段私有，不能由model参数直接拼出。

RepositoryObservation至少记录source RootIdentity、绝对git-dir与common-dir的目录身份、object format、完整HEAD commit OID、symbolic HEAD（若有）、clean状态及诊断理由。Prepared记录创建请求绑定、父目录身份、唯一leaf/branch、源identity+expected_head+common身份、策略版本和有效期限。DTO公开安全诊断码，不透出原始config/环境/stderr密钥。B负责持久intent/receipt/CAS/claim/新Project登记，C负责严格HTTP/CLI DTO和显式用户入口；D负责Git参数、观察/执行/清理及有限进程；workspace_identity扩展如必要独占D，lib私有mod仅D，git_diff现行行为只读保留，提取共享runner须另授权而不顺手重构。

所有命令Command::args逐参数构造，不经pwsh/cmd、不让用户提交选项。安全Git可执行文件从主机可信配置/绝对PATH项解析，拒绝source、destination和任何项目可写目录内git.exe；现git_diff.rs:78只排除本root，不等于全机可信二进制保证。路径必须绝对本地盘、合法UTF-8；leaf严限ASCII，拒绝保留设备名、点/空格尾、ADS、UNC、..、重解析/大小写敏感目录。产品不硬编码本轮E盘限制。

解析HEAD建议固定rev-parse --verify --end-of-options HEAD^{commit}，校验40/64位十六进制并与object format一致；随后cat-file -t精确commit。只用固定HEAD输入，最终创建传已解析OID，不重传可能移动的分支名。若允许refs，check-ref-format只校验完整refs/heads/...或refs/tags/...，不要使用有@{-1}展开语义的自由--branch输入；拒绝短名歧义、选项前缀、reflog/路径表达式，解析完整ref后固定OID。创建采用受控worktree add --no-track -b <new-branch> -- <absolute-target> <oid>（具体Git最小版本与参数次序在实现时通过真实fixture验证），不用-B/--force/--guess-remote/orphan。

## 配置与checkout副作用是首要冻结点

现git_diff.rs:95环境清空、禁全局/system config、禁协议/lazy-fetch/replace、关闭fsmonitor/hooks/credential/diff.external，读取blob绕过checkout，因此现有安全属性不能直接迁移为checkout安全。worktree add可触发post-checkout、reference-transaction hook、.gitattributes指定的smudge/process filter（含LFS）、fsmonitor及配置包含路径；普通“只本地Git”仍可能执行外部程序/网络。

创建runner至少继承现环境净化，固定禁协议和lazy fetch、replace、fsmonitor、自动维护、submodule递归、远端猜测与branch自动track；hooksPath指向Host拥有且不可被repo写入的空目录，不凭相对空值假设hook禁用。禁用户/global/system attributes来源，repo attributes仍需处理。不能以GIT_ALLOW_PROTOCOL禁用Git transport等同禁止filter自行联网。

最小支持策略建议对执行配置采用fail-closed：无include/includeIf、filter.*、promisor/partialClone、对象alternates/共享外部object pool、sparse/worktree config未知扩展、submodule gitlink、symlink树项时才允许首片自动checkout；发现则Unsupported并保留只读诊断。config枚举必须--no-includes，不读取包含文件内容到响应；同时核common config与config.worktree。base tree用ls-tree -r -z有界解析，检查mode 120000/160000；有这些模式拒绝，不自动初始化或将symlink伪装普通文件。

即便策略预检通过，外部进程仍可能改local config/refs/父目录，预检+复核不能成为OS对抗性沙箱保证。更稳方案需冻结每次Git调用使用的受控配置来源、覆写所有外部执行能力，并对Git版本实证；若无法证明checkout过程中不会重新读到未受控filter/include，必须明确仓库受信任假设或推迟自动checkout，不能声称完成任意恶意仓库安全隔离。仅用--no-checkout可缩小风险但不满足可直接编码工作区闭环，不推荐把空目录冒充完成。

## clean、唯一性和身份

创建只复制固定commit，不复制源工作区/index/untracked/ignored。首片要求源tracked/index和非ignored untracked clean（status --porcelain=v2 -z --untracked-files=all，禁fsmonitor/optional lock，输出溢出拒绝）；ignored构建产物可存在，明确不纳入新树。dirty拒绝并说明未提交内容不会带走，不帮用户stash/add/commit。当前主仓管理dirty是本次只读许可，不推导未来创建接口允许dirty。

clean与expected HEAD在prepare、claim前、spawn前复核；新树成功后再核HEAD/source identity。clean只是观察，外部编辑不会被Host锁住；发现变化则Conflict或spawn后Unknown，不“修复”源目录。Host用common-dir身份作为仓库级操作串行键，不能仅按两个Project ID各锁一次。DB UNIQUE预留规范化目标路径/branch；Git自身拒绝分支存在/已占用，不能靠预检查代替最终竞态裁决。source root及新目标不得与任何登记Project根重叠，新树不能放源仓内。

目标父目录必须已存在并RootIdentity capture/verify，leaf必须不存在（哪怕空目录也拒绝）；同一路径大小写别名按目录身份和Windows比较策略处理。新目标成功后capture RootIdentity，并确认.git为普通文件、git-dir/common-dir与预期对应、worktree list --porcelain -z有唯一记录及正确HEAD/branch。linked .git是指向common管理目录的指针，不能把它当任意安全普通文件；额外记录per-worktree git-dir/common-dir目录身份以及marker解析结果，后续诊断复核。workspace_identity.rs现已提供capture/verify/overlap与祖先重解析检查，但仅观测不持句柄锁；缺失目标要基于已存在父目录，不直接capture尚不存在的leaf。

## 持久claim、unknown和人工诊断

流程：只读prepare→B事务持久intent+唯一预留→用户明确创建请求→B事务复核/claim→owned Git worker→Git postcheck→B事务完成receipt+登记新Project。不跨Git等待持Store锁；claim后worker持有内存执行权，caller取消只停止等待。claim前可失败验证尽量完成，任何spawn必须在durable claim之后。创建Project本身不隐式选中/授grant/迁移旧任务。业务登记事务失败但Git已成功同样Unknown，不自动第二次add。

建议状态Prepared/Claimed/Created/Unknown/Rejected（经理与B最终冻结）；Created必须Git实际核验与新Project登记成功，receipt记录固定OID/branch/path/repo身份。Git非零也可能已创建branch、管理目录或部分文件，spawn后的exit!=0/timeout/reader溢出/丢caller/Host crash不能一概视为无副作用。直接child kill+wait不撤销已经写出的refs/目录。重复key只读旧结果及当前诊断，不自动重新运行；Unknown重启仍Unknown，不因目录不存在就重试。pre-spawn明确未执行可报告Rejected，但不得释放同key后悄悄再执行。

进程借鉴command_execution.rs的owned worker+统一deadline+固定5秒cleanup与typedunknown，但建议独立Git错误类型GitWorktreeOutcomeUnknown，不复用shell审批错误。git_diff.rs:172现run_command超时后child.wait无额外期限、错误被映射Unavailable；它适合现有只读语义但不可直接承接创建。bounded stdout/stderr不泄密，正常Git输出为证据不作命令；超限也必须终止/等待，所有回收共享预算。Host强杀不能保证后代/锁清理，禁止删除.lock猜测恢复。

诊断仅读git worktree list --porcelain -z、show-ref/refs、固定HEAD对象、目标存在/身份/.git/admin路径及数据库receipt。返回PresentVerified/AbsentObserved/Partial/Conflicting/IdentityChanged/Unavailable等观察，不自动把Unknown转Created或重新claim。要“接纳已完整创建的新树”应是后续明确独立操作，绑定新观察版本和相同operation，重新授权登记，不能假称重放原Git动作。提供可审阅路径/OID/branch/时间与脱敏诊断；人工自行选择后续Git操作，本片绝不自动remove/prune/repair/unlock/branch-delete/reset/clean/stash/merge/push。

## 必须先冻结并验收的反例

1. 真实临时仓库创建后独立HEAD/index而common共享；新Project不继承Task/grant/checkpoint，源HEAD/index/工作文件未变，refs新增恰一条是明确允许的共享副作用。
2. HEAD在prepare后移动、dirty/staged/untracked/冲突index、unborn/bare、existing branch/path、大小写别名、相互重叠Project、父目录替换/reparse全部fail-closed；并发两个同branch/target仅一claim获权。
3. 可执行post-checkout/reference-transaction、smudge/process、includeIf、fsmonitor、remote helper、partial clone fixture用marker/socket证明没有执行/联网；仅断言参数含-c不算实证。恶意repo git.exe、GIT_*环境、global hooks与attributes不能劫持。
4. 完整commit SHA-1/SHA-256（若支持）、tags到commit的限定解析、非commit OID与rev注入拒绝；Windows路径与branch参数分界验证，不借shell转义。
5. claim前拒绝无Git写，claim后spawn前真进程kill+wait恢复Unknown不自动重试；add中途/成功后落库前故障保留branch/目录/admin残留，不自动清理。诊断与重复key零副作用，误匹配同名目录不得接纳。
6. caller abort后worker还在、总deadline涵盖pipe、收尾失败typedunknown、输出上限、常规非零partial side effects；使用true runtime停止/子进程kill+wait而非drop Arc声称crash。
7. 源/new root及common/admin路径身份变动、外部refs/worktree注册变动按观察冲突；状态不得宣称全仓库隔离或抗管理员TOCTOU。

建议owner：D独占git_worktree.rs及其模块/对应集成测试、必要workspace_identity增强；B独占intent/schema/claim/idempotency/Project登记/恢复/Engine装配；C独占CLI/HTTP DTO/只读诊断交互与真Host验收，经理冻结跨文件API及序列整合。是否允许受信任仓库假设、受限config策略、dirty策略、新Project默认配置/授权、Unknown人工接纳是开工前产品决定；本报告不自行派发实现。

限定预审完成，无实现或测试结论。仅本人档案已更新，当前冻结待经理完整契约及有效新任务；不创建真实工作树或执行任何Git写。