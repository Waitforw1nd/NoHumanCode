# NEXT-13 独立Git工作树持久与装配组件提交报告1.0

2026-09-28，员工B/GPT-6-astra；执行B-R12-01-20260927-E01。原代理接续，不另开执行者；PRE、B-R11及全部历史报告保留。此件为B组件交付，等待经理review及C最终组合门禁，不宣称NEXT-13整体APPROVED。

## 固定候选、所有权与整合窗口

独立树`../nhc-b-worktree-management`、分支`codex/b/worktree-management`；基线32b9bfaca59a3f87444d20ed705d01d4678e7f46。B最终固定44307ed78171073c58e4e499d5758495df916dc3，NoManCoded07b208e1a9c69e536847dbaa57cf1d324142cc3、Rust20bcd168e5a60d872e11170b79111c721a98f1f2。交付source/index空、无构建、源码冻结；源码提交人B，跨树整合人经理，未自行merge/shared index/push。

已消费经理整合的真实D四参diagnose及离线/dirty观测修复；四个Engine公开API、公开DTO拼写不变。B仅修改worktree_management/store/repository/project_config/engine/lib以及经理扩权的旧fixture测试部分；git_worktree/workspace_identity由D独占。授权旧fixture改动包括plugin_management的cfg(test)及approval_gate/checkpoint/project_context/project_migration/workspace_changes。档案只主仓本目录。

## 最终行为

新key先只读盘点本DB完整Project根集合，D验证本地clean显式HEAD并归并common-dir；Immediate事务重读幂等/完整盘点/配置/根快照/同common全部成员活动与未决副作用，原子持久claim、生成ID/branch/目标计划和完整成员预约。调用方取消后已spawn的owned worker继续；Git真正执行仅在持久claim之后，成功Project默认无权限、无选择切换、无Provider请求，origin和succeeded在同一事务提交。

所有state相同key纯回放在当前Git/配置检查之前；异参冲突、跨source读取NotFound。unknown保留Project/common预约；startup只claimed→unknown且重复recover不漂移，不补Git/清理/登记。diagnose从严格保存快照构造含全部根的信任上下文，离线根不被删掉以放行Git；succeeded保存的root/common/admin结构关系和物理身份均复核，再由D用实际目标admin与保存admin比较。观测不改变持久状态、预约、Project或权限，即使PresentMatching也不finalize。

普通登记在活跃worktree operation期间暂停；原登记key回放仍优先。反向new Run/Turn、resume、approval新增/claim/approve/ack、restore claim、checkpoint commit都在原事务内查预约；拒绝、取消、finish/result/recovery能收尾。保留旧binding冲突、Checkpoint Conflict及恢复封存优先。此保证范围是同一个数据库内，不能扩称不同DB或任意外部Git进程的全局互斥或OS沙箱。

## Schema14完整新增DDL及校验

marker为`git-worktree-operations-v1`。以下为实际规范SQL（非概念结构）：

```sql
CREATE TABLE worktree_operations (operation_id TEXT NOT NULL PRIMARY KEY, idempotency_key TEXT NOT NULL UNIQUE CHECK(typeof(idempotency_key)='text' AND length(idempotency_key) BETWEEN 1 AND 200), source_project_id TEXT NOT NULL REFERENCES project_configs(project_id), requested_project_id TEXT NOT NULL UNIQUE, request_json TEXT NOT NULL CHECK(typeof(request_json)='text'), request_digest TEXT NOT NULL CHECK(typeof(request_digest)='text' AND length(request_digest)=64), plan TEXT NOT NULL CHECK(typeof(plan)='text'), members TEXT NOT NULL CHECK(typeof(members)='text'), common_key TEXT NOT NULL CHECK(typeof(common_key)='text' AND length(common_key)=64), target_path TEXT NOT NULL UNIQUE, branch_ref TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('claimed','succeeded','rejected','unknown')), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>0), receipt TEXT NOT NULL CHECK(typeof(receipt)='text'), UNIQUE(common_key,branch_ref));
CREATE UNIQUE INDEX worktree_active_common ON worktree_operations(common_key) WHERE state IN ('claimed','unknown');
CREATE TABLE worktree_project_reservations (project_id TEXT NOT NULL PRIMARY KEY REFERENCES project_configs(project_id), operation_id TEXT NOT NULL REFERENCES worktree_operations(operation_id));
CREATE TABLE project_worktree_origins (project_id TEXT NOT NULL PRIMARY KEY REFERENCES project_configs(project_id), operation_id TEXT NOT NULL UNIQUE REFERENCES worktree_operations(operation_id), source_project_id TEXT NOT NULL REFERENCES project_configs(project_id), identity TEXT NOT NULL CHECK(typeof(identity)='text'));
```

规范13及shell marker先验证；Immediate事务内再读version/marker，建立对象并校验后marker/version同commit。现行14重开严格DDL/CHECK/FK与完整持久行：request重hash、规范UUID/HEAD/branch/target/time/revision/state/project/cause、plan结构/root/common关系、完整member根快照和精确预约集合、succeeded origin Project/source/root/admin/common关系；孤儿FK拒绝，不自修、不清空旧业务。

`members`保存所有已领取成员及RootIdentity，因此不能只剩source的一条预约便冒充完整claim。origin.admin必须是对应common/worktrees直属、与source admin不同且与结果根/common不重叠；不假定Git admin basename等于生成leaf（可能有合法后缀）。仅目录存在仍不足以判定admin对应关系，第四参实际观测比较封住同common另一棵真实worktree admin互换。

## 准入与故障覆盖映射

| 要求/入口 | 实际组件与断言 |
|---|---|
| W01创建与默认配置 | success_defaults_replay_and_diagnosis：真Git/源HEAD与index不变、目标branch与同common创建、默认ProjectConfig、selection不变、network_authorizations/tasks/runs为0 |
| W03原key/异参/跨source | all_terminal_replays覆盖claimed/rejected/unknown；success覆盖succeeded；dirty/配置变化后回放不改refs；same_key双Store同receipt一创建一回放；different-key双Store精确一Busy；source_scopes读NotFound/异source重用key冲突 |
| W03输入与成员完整性 | stale config/head零operation/目标；两真实同common Project领取完整2条预约，删除sibling行重开拒绝 |
| W03新任务先赢 | reverse_actual_running_task：真实运行Task等待审批时worktree claim Busy；实际cancel和worker结束后可claim |
| W03工作树先赢newRun/newTurn/resume | reverse_new_turn_*分别调用start_in_project、send_chat_turn、Store.resume_task，逐个Busy且所有业务表无漂移 |
| W03approval ensure/approve/claim | reverse_approval_*：旧ensure只读回放，新ensure与approve Busy；错误args仍BindingConflict；claim最终guard Busy、NotStarted不变；deny允许 |
| W03command ack | reverse_command_ack_*：旧已ack批准纯回放，新ack Busy且ack行/审批/event不漂移，新command ensure Busy；deny允许，Provider计数不增 |
| W03restore/checkpoint/register | reverse_new_turn_*：restore/create_checkpoint/新普通register Busy且业务全表不漂移；原登记key回放；worktree Rejected释放后checkpoint正控成功 |
| W03登记竞态 | registration_inventory_and_claim_race：两个Store三轮真实并发，只能登记赢→claim Conflict或claim赢→登记Busy，无漏预约新增成员 |
| W03旧优先级 | reverse_checkpoint_*原keyreplay/异keyConflict，resume保持Checkpoint Conflict；reverse_existing_restore_*精确旧receipt回放，resume保持Sealed |
| W06owned worker | dropped_caller：ClaimCommitted停点后abort调用者，release后后台恰一次succeeded/Project，key回放不重复 |
| W07迁移/损坏 | canonical13非空Project/config、plugin rowid/receipt与opaque密文全表逐值保全；marker故障全对象回滚；两个并发open都14；坏index/marker/version/DDL、孤儿origin FK、坏request/plan/成员/receipt无自修；旧11/12实际业务全行迁移另过 |
| W08真实崩溃 | real_process_three_durable_stops：claim后spawn前、Git后登记前、登记后响应前真实子进程ready，父kill+wait exit1；recover分别unknown/unknown/succeeded，前两预约保留；replay与多次inspect不重复Project/Git，源HEAD/index不变，network_calls/tasks/runs为0 |
| W10身份与只读观测 | 保存admin被合法source/root/common快照替换拒绝；保存目录物理替换IdentityChanged；两真实linked cross-admin互换IdentityChanged；source移走IdentityChanged、无关Project离线Unavailable；原receipt/预约不变 |
| W11旧兼容组件 | approval_gate35、checkpoint15、project_context7、project_migration3、workspace_changes21；lib schema21及旧b02两项+修订一项，详见逐次候选 |

上述approval claim最终guard使用受控持久Task JSON running注入，专门触达防线；不会把它声称为合法completed→running业务路径。实际业务生命周期拒绝非法转换的首次失败保留。六类cancelled未决恢复/写反例使用真实表结构的受控持久状态fixture，并非六次实际OS恢复故障；真实running/cancel、恢复seal与实际完成write另由Engine组件证明。相同/不同key以及普通登记竞争是真实两个Store/线程，不扩称跨数据库互斥。

W02独立grant、新Project显式Run以及完整HTTP/CLI由C负责组合验证；B证明默认无权限及旧根/业务行保全，不冒称代C执行。W04/W05及进程deadline/管道/恶意Git配置实证由D负责，B只消费其真实实现。最终五门禁由C唯一执行，B未运行或宣称通过。

## 实际命令、候选与结果

| 证据名 | 实际命令 | 实际HEAD | exit |
|---|---|---|---|
| compile-01 | `cargo check --lib --locked` | `e7a92c2` + dirty | 101 |
| compile-02 | `cargo check --lib --locked` | `2098f0e` + dirty | 0 |
| worktree-01 | `cargo test --lib worktree_management::tests:: --locked -- --nocapture` | `3a22df4` | 101 |
| schema14-01 | `cargo test --lib n13_schema_ --locked -- --nocapture` | `b2a02ab` | 0 |
| worktree-02 | `cargo test --lib worktree_management::tests:: --locked -- --nocapture` | `e1a798b` | 101 |
| worktree-03 | `cargo test --lib worktree_management::tests:: --locked -- --nocapture` | `f30a08e` | 0 |
| reverse-01 | `cargo test --lib n13_reverse_ --locked -- --nocapture` | `13ae5bd` | 101 |
| reverse-02 | `cargo test --lib n13_reverse_ --locked -- --nocapture` | `6b4a9f2` | 101 |
| n13-components-01 | `cargo test --lib n13_ --locked -- --nocapture` | `084da4a` | 0 |
| old-fixtures-01 | `cargo test --locked --test approval_gate --test checkpoint --test project_context --test project_migration --test workspace_changes -- --nocapture` | `084da4a` | 101 |
| old-fixtures-02 | `cargo test --locked --test checkpoint --test project_context --test project_migration --test workspace_changes -- --nocapture` | `8bbec88` | 0 |
| schema-old-01 | `cargo test --lib schema --locked -- --nocapture` | `8bbec88` | 0 |
| old-b02-01 | `cargo test --lib b02_ --locked -- --nocapture` | `8bbec88` | 101 |
| old-b02-02 | `cargo test --lib b02_corrupt_shell_rows_fail_without_repair --locked -- --nocapture` | `417b66a` | 0 |
| fmt-owned-01 | `rustup run stable rustfmt --check --edition 2024 --config skip_children=true` | `417b66a` | 1 |
| fmt-owned-02 | `rustup run stable rustfmt --check --edition 2024 --config skip_children=true` | `44307ed` | 0 |

主要结果：b2a02ab schema14首专项4/0；e1a798b worktree-02为6/2；f30a08e worktree-03为10/0；13ae5bd reverse-01为4/2、6b4a9f2 reverse-02为5/1；084da4a完整n13组件24/0（其后新增FK在8bbec88 schema-old21/0中通过，故最终B新增独立用例25项，不冒称在443重跑25）；084旧integration首approval35/0、checkpoint11/4后停止，余三目标未执行；8bb修后checkpoint15/0、project_context7/0、project_migration3/0、workspace_changes21/0。8bb旧b02为2/1；417b66a只修鲜库版本断言后单项1/0，已过2项未重复。443只移动lib模块声明格式，fmt-owned复验0，不冒称该排序提交重跑全部cargo。

## 全部失败、修复与未执行

1. compile-01 exit101：B read_receipt guard类型错误、unused import，后修复compile-02 exit0。首次新增测试worktree-01 exit101是漏Store import七处同根编译错误，未执行Git链；随后真实lib-test编译通过。
2. N13-B01经理静态发现partial restore/outcome与workspace change未知漏检；补入同成员事务准入。N13-B04初稿runs.value不符真实schema由C/经理静态发现（未冒称曾执行），改真实列；动态六类反例通过。
3. N13-B02/03源自C及经理静态交叉review，初稿admin关系不足和诊断在线预检问题修订。worktree-02实际2失败：happy创建已成功，但后续configure因fixture无settings；无关Project离线被D分类Partial。分别补settings与经理协调D分类后通过。经理再发现cross-admin，增加真实两linked反例及D第四参，组合通过。
4. reverse-01/02失败是旧生命周期正确拒绝测试非法completed→running、以及checkpoint测试key空格违反旧输入约束；分别明确注入测试状态和改合法key，保留原优先级，没有改产品以迎合测试。
5. old-fixtures-01 checkpoint四失败是规范迁移产生三张新空表，而旧全表预期未增加；保持原完整快照，只显式增加精确三空表，未扩大过滤/删旧检查。old-b02-01鲜库user_version原断言13需改14；历史库仍先真实drop新表/marker再重建，未用只改version冒充历史。
6. fmt-owned-01 exit1：lib Git模块声明顺序；443格式排序，fmt-owned-02 exit0。此前一次生成测试SQL引号导致rustfmt parse错误、一次Python默认编码读文件失败均只会话输出，无独立raw日志，在0.1检查点已如实记录，不编造文件证据。
7. 最终Clippy/全测试/wasm及完整组合check未由B执行。报告收尾时经理已通知C实际接收最终组合935a27c43c8661ab83a4a65b7df8c328f4884fe1（NoManCode d2ff69a800beb21487a40e8a268544e24c1a549c、Rust c48c78b987fb6af018e25f2cf8e37e4ee4f97340）并开始正式串行五门禁attempt1，结果尚未由本报告确认。B最近组件测试并未包含D后来Box/盘根修订，不称其在该最终组合受测。当前D/C自己的历史失败不归B日志，也不覆盖它们的原件。

## 证据发布与下一步

[完整证据manifest](证据1.0/manifest.json)含16组log/json共32份原件与发布副本逐份SHA256、真实UTC/命令/HEAD/status、具体路径转换记录；原件在`../nhc-b-worktree-management/.local/review-evidence`，TMP/target始终本人E树.local，既有MSVC只读使用。本次发布仅仓库路径相对化与UTF-8文本规范化；原始文件保持，工具安装路径作为当次事实不冒充可移植配置。0.1检查点与证据不覆盖。

源码/档案交付后冻结；下一第一操作为经理review/组合整合后的明确修订或验收反馈，不自行运行最终门禁或跨树merge。
