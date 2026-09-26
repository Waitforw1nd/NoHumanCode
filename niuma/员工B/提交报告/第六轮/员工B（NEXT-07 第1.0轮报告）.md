# 员工B：NEXT-07 第1.0轮实现与定向验证报告

2026-09-27，员工B / GPT-6-astra，执行 `B-R6-01-20260927-E01`，接收时间2026-09-27T03:11:26.1880269+08:00。当前产品候选 **f28218815ee7280a6eca77d20c685e085bb47381**，Rust tree **8056d1aece8fad1b18db41bb5df67e052be5dfa1**。独立树`../nhc-b-network-authorization/`、分支`codex/b/network-authorization`，本人范围提交；共享index/跨树合并仅经理。源码status空、无后台构建，冻结待经理review及C/D最终组合；本报告不是最终五门禁或验收声明。

## 版本与交付

初始c5430b1；安全DTO4b26377；经理合D首transport为4e5b789；Store/schema10检查点0cce2bd；经理合D43fc6ee为36e96a6；真实API9be2bdd；经理组合C/D为bce914a；安全ref/slot修正2009649；本候选f282188。当前任务同一执行者接续，无新代理、无旧任务恢复。

源码为B负责的network_authorization/Engine/Store/repository/approval/domain/lib；新增真实network_authorization集成测试及tests/support/network_fixture.rs。旧B所属AP/Checkpoint/runtime/Turn/workspace/crash等只补明确授权setup、command ack、schema10预期；runtime模型变化行为顺序经经理明确确认调整：先保留旧same ID/role/model/usage22全部断言，再修改模型断言旧Task Stale且无新调用，保留URL拒绝。AP重启mock复用原端口，避免重写旧Task网络绑定来绕过新规则。live只改为真实Engine授权入口以通过编译，从未运行。

实现包括：schema10严格DDL/列/FK/index/marker验证；DPAPI凭据snapshot、实际source ref及revision；preview/confirm/revoke/query；同key原授权事实回放且完整归属/绑定重核；Task创建/新Turn与具体grant绑定同事务；所有发送在实际slot后短gate重核、持久claim并构造不可复制permit；完成/失败/unknown及同ID恢复；管理models/check/balance；configure/账号纯本地原子保存；旧Task不可自动换绑；命令notice/ack sidecar与原v2摘要隔离、决定与claim事务复核、缺ack继续等待。

安全credential_ref采用`stored/route/<id>`、`stored/legacy/<id>`、`stored/account/newapi`、`env/<name>`，不放宽全局秘密检测。slot获取统一在gate内try_acquire，Notify先enable再查slot，等待不占gate、不在gate外临时占permit；RAII先release再notify。配置在管理调用在途时明确拒绝，容量只在commit后按已记录总容量调整，避免available误当总量及下调竞态。

## 经理问题收口与实证

| 问题 | 实现/证据 |
| --- | --- |
| N07-B-D01 | IF NOT EXISTS仅从完整DDL头移除；真实伪索引`state='activeifnotexists'`拒绝，合法active重复INSERT由SQLite约束拒绝 |
| N07-B-D02 | confirm replay核subject、两revision、digest、purposes；FK合法但跨subject错链返回Corrupt，同时revoked原决定重放不复活 |
| N07-B-D03 | 真实`TEXTNOT NULL`列可插NULL且旧verify错误接受，修正fixture后先红；增加table_xinfo与内存标准DDL列name/type/notnull/default/PK/hidden对照后绿，原literal/FK/index校验保留 |
| N07-B03 | 短gate内slot获取与配置容量调整串行；真实持有listener时配置拒绝；内部hold gate/poll Pending验证获取窗口、Notify释放、下调后单槽、取消等待无泄漏 |
| N07-B05 | receipt集中核success/result/http_status、purpose/subject/grant用途、method、origin、时间；真实仅改合法JSON字段组合返回Corrupt；finish写失败返回同ID unknown，库claimed恢复unknown |
| C preview Invalid | `secret:...`旧显示ref误触敏感赋值检测；改安全source编码，真实stored secret正对照通过，不用env夹具绕过 |

## 定向验证（非最终组合五门禁）

以下是本轮迭代过程中最终有效组件结果的去重汇总，共 **180项**；不宣称它们来自同一次最终组合全量命令。最后源变更后network16与限定Clippy通过；最终所有组合回归仍由C运行。

| 实际测试/检查 | 最新有效结果与日志 |
| --- | --- |
| network_authorization | 16/0，network-final-02.log |
| approval_gate | 32/0，final-directed-02.log |
| runtime | 10/0，final-directed-02.log |
| checkpoint | 12/0，regression-04.log；schema9全列进一步增强后1/0，schema9-final.log（计入12不重复计） |
| session_turns / tool_session_turns / workspace_changes | 19/0、10/0、21/0，regression-03.log |
| workspace_crash | 6/0，regression-04.log |
| assessment_adversarial / final_acceptance | 4/0、9/0，regression-02/03.log |
| Store / Repository单元 | 11/0、13/0，store-unit-01/repository-unit-01.log |
| Engine workspace_phase_tests | 14/0，phases-01.log |
| Engine network_gate_tests | 3/0，gates-03.log（含两个真实kill子窗口和真listener正对照） |
| 标准check | bce914a组合check04 exit0，非最终候选五门禁 |
| 限定Clippy | clippy-final.log exit0，lib及本人10个test targets；含live仅编译，从未运行 |
| fmt / diff-check | 指定本人文件rustfmt及build.ps1 fmt、git diff --check通过，最终提交前检查完成 |

N01/N02：真实Task/secret/listener有效，无grant/用途不足零TCP，无部分check；明确confirm后同fixture真实models/check/Task成功；start/send key重放无网络；confirm/Task/new Turn绑定触发器失败事务零新增。append失败对数据库所有表所有列逐字比较，新明确Turn可绑定新grant，旧Task不能换绑。

N03：实际stored secret变更、raw settings更换目标、模型改变、两listener端口、实际env source/value跨真实进程变化均重核；same-value env重启可用，值/引用变化拒绝；账号user变更须新token，仅quota不变更身份授权。

N04：真实排队请求在slot后才准入，撤销先赢无call/零TCP并同listener有正对照；permit先赢使用Notify控制真实listener持有响应，撤销不等待网络，该在途完成后新准入拒绝；Task已准入completion返回工具调用后，撤销阻断下一模型轮（真实listener仅一次请求）。没有用随机sleep race证明顺序；小timeout仅用于观察已确定拒绝后的零连接。

N07：真正子进程分别在network claim前及commit后阻塞，父进程kill并wait；前者无call，后者同call ID恢复unknown，重复recover事实不变、listener零重发。另结果持久化失败触发器证明同IDunknown（库暂留claimed再recover），不生成第二ID。C负责真Host发送后及完整响应结果持久失败窗口，见C自己的证据；本报告不将C事实计作B实跑，也不把B kill后未send当send后窗口。

N08：真实run_command缺ack拒绝、listener零请求；显式ack后真实PowerShell命令访问本地listener，notice明确outbound_sandbox=false，后续模型轮完成。旧approved缺sidecar补ack只增sidecar/独立事件，原approvals所有列（v2/决定人/时间/preview）完全相同，重复ack无新增事件；unknown不补发。

N09：真实文件写入、审批、Checkpoint、restore后退为schema9，保留旧业务全部表全部列（含before密文/restore事实）及Checkpoint全部列；两次open到10后完全相同。旧6/7/8连续迁移、真正schema8业务/密文保持、伪约束/index、无marker拒绝都继续通过。schema7迁移期间尚无schema10表时ack展示按Required处理，不自动批准历史命令。

## 失败、界限与日志

28份日志原件位于本人树`NoManCode/rust-app/.local/review-evidence/`，发布副本在[第1.0轮证据](第1.0轮证据/)，原始/发布SHA256逐一见[hashes](第1.0轮证据/hashes.md)。原件不改；发布仅相对化工作树路径，系统临时路径显示`[system-temp]`（实际测试使用当时OS TEMP，不冒称E盘临时目录）。发布文件并非原始字节，双hash区分。

保留失败：初次直接cargo check link.exe缺失（只有工具回执）；check01脚本工具链发现失败；check02本方接入遗漏+旧C依赖31错，check03仅旧C依赖11错，check04组合过。network01因旧secret:ref误拦8失败；network02/03修正后8过。gates01因测试Mutex排队顺序错误被明确中止，gates02/03修正后3过。regression01旧setup/ack/迁移fixture失败，regression02旧schema8快照多出新空表比较失败，regression03旧runtime语义及workspace_crash setup时点失败，均保留原日志并按上述修复。final-directed01新测试字符串replace参数/未定义helper编译失败，已修。clippy01 nested if、clippy02含本方fixture问题及C type_complexity，后者通知C自行修；限定clippy03/final过。schema-type-before.log最初伪DDL还遗漏subject NOT NULL而误绿，不能作缺陷证明；schema-type-before-corrected.log才是严格单点修改的先红事实，后续network16包含修复后绿。

尚待经理review、C/D串行组合及C唯一最终五门禁；不能据本组件结果宣布NEXT-07完成/发布。未运行release、付费live、push，未重写旧批准/v2/Checkpoint业务数据，不改他人源码。下一第一操作保持源码冻结，按经理具体review支持或组合编译问题修本人范围。