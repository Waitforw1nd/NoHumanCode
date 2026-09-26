# 员工B：Task-before Checkpoint与Engine接入 第1.0轮报告

2026-09-27，员工B（GPT-6-astra），执行`B-R5-01-20260927-N06`。接收时刻`2026-09-27T01:33:25.5774948+08:00`，中断后于`01:56:44.8624569+08:00`接续同一任务/模型；依据[统一契约及经理增补](../../../项目经理/任务/2026-09-27NEXT-06真实Diff与最小Checkpoint契约.md)。旧轮次结项、旧树与九继承档案保留。

## 固定候选与状态

- 基线`7a4a001d4296a26086d5196997ae80ac88e1c22b`；独立树`../nhc-b-checkpoint/`，分支`codex/b/checkpoint`。本人只提交本树授权源码；经理是整合人及共享index唯一操作人。
- API检查点`725f70aecd5568844bc0e650b82d9bb03cfdff6b`；N06-B01/B02加固与测试`d859c0a70f378aba779fa516f396a4a369db5c61`。
- 经理串行合D `0def34f75564437371f399b19330a206e06f8fd5`为`5f17df3d1ecac1c28e3b4729ff0eae9ed7b022a5`后，B完成真实git_diff Engine入口。
- 最终B实现候选`99ca2b9e9f2233b3556aec32ab69e3754ad196b7`，Rust tree `c642a7896079736967103ca367019b148476e709`。提交后源码干净冻结，全部构建退出，无后台cargo/rustc。档案写在共享本人目录，不写工作树档案副本。
- 当前为提交待review/联合验证，不自称验收。D的cfg(test)可见性警告由D独占修，经理另合其补丁；C是最终组合五门禁唯一执行者。B未执行release/live/推送。

## 实现与公开接口

`checkpoint.rs`公开契约DTO及`CheckpointError::{Invalid,NotFound,Conflict,Unrestorable,Corrupt,Internal}`；Engine三Checkpoint方法和git_diff按冻结签名。新增正式安全事件`checkpoint.created`是经理单独授权的domain allowlist一行，创建事件在同一事务仅一次；查询不追加事件，恢复沿用原file_restore/同一receipt。

schema9新增checkpoints：Task唯一、创建key按Task作用域、kind/task_before与generation/1 CHECK、Task FK。完整DDL核验保留字符串字面量，防止伪CHECK被空白归一化掩盖；schema9无迁移marker拒绝修复。schema8→9 DDL/marker/version事务提交；旧业务行不改写。

创建先拿共享gate，检查completed、当前Host根、同workspace活跃Task、legacy/unknown/未决审批/已有restore、路径数与before总量；逐项解密并核摘要、校验真实调用/审批binding、当前所有文件匹配最后after。SQLite immediate事务重查Task/源记录/未决状态与幂等。manifest冻结有序源change身份摘要（覆盖Task/root/scopes/binding/path_key、before密文身份摘要与前后digest/state）及最早before/最后after条目；密文仍留原受保护记录。同key回放原ID，其他key冲突，不触发模型/工具/Git/文件写。prepare_workspace_change与resume_task拒绝已checkpoint旧Task。

查询校验manifest摘要、合法字段、Task根/scopes和完整源集合，重算每路径条目；不能用新源重建损坏manifest。合法restore_state/outcome变化不纳入不可变源摘要。Checkpoint恢复和兼容Task恢复都验证已有checkpoint，随后复用原预检/claim/逐路径outcome/partial/unknown路径；gate不递归锁，完整回执回放不覆盖恢复后外改。

真实Git读取由D实现；B的Engine wrapper持同gate，检查Task存在、当前Host workspace绑定、同根活跃Task，返回原typed WorkspaceChangeError或D GitDiffError。调用真实`read_git_diff`，不从changes合成patch。lib两模块导出由B唯一完成。

## 验证矩阵与证据边界

| 项目 | 本轮B证据 |
| --- | --- |
| K01 | 真实批准write：修改+新建+同路径两写；持久重开查询、最早before/存在性精确恢复；其他文件及Git HEAD/index原字节不变；complete后外改再恢复不覆盖 |
| K02 | 同Task同key稳定回放、不同keytyped Conflict；真实gate下两个创建先poll Pending后收敛；创建INSERT故障零checkpoint/事件；不同真实Task同key生成独立ID；旧Task resume拒绝。完整同Session新Turn由C联测，B既有N07库回归仍通过 |
| K03 | 外改Conflict；before解密/manifest/源摘要/根scope损坏Corrupt；unknown和legacy Unrestorable；空Task/坏keytyped拒绝；129路径、33×256KiB before（超过8MiB）真实批准后创建零事实。旧跨Task修改/Host根/scope安全测试21项回归通过；不把旧Task恢复证据冒称每个Checkpoint新入口均另造夹具 |
| K04 | 两条restore真实gate Pending后同receipt/同claim；多路径预检失败零写；第二路径执行前测试注入失败产生partial、第一路径已恢复、重复Task入口同回执、禁止新Turn。该注入不是磁盘硬件故障；C补Windows真实Host层场景 |
| K05 | 新库/schema6/7/8→9与重复open；伪CHECK字面量空格/缺generation/缺unique/缺marker拒绝；真实schema8成功write+restore的所有业务表所有列逐值保持，含密文before_blob；旧AP/W迁移夹具移除新表/marker后保留原目的；敏感before不入DTO/events |
| K06 | 从checkpoint入口进入三真实子进程停点：claim前、持久claim后/文件前、文件效果后/outcome前。parent kill+wait，重开并显式recover；已claim→unknown、同checkpoint身份、无自动重试、旧resume与新Turn拒绝。claim前无claim可显式恢复；C另证HTTP/CLI与实际Host层 |
| Engine Git | changes为空仍从真实Git index/磁盘读取三视图；Task不存在/Host根改变/活跃Tasktyped错误；真实poll Pending证明diff和restore同gate。D的15项Git模块验收属于D报告，不计为B新测 |

## 命令、结果与版本

均在独立树Rust目录执行，使用原D盘`target`；进程设置`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`、`CARGO_INCREMENTAL=0`，没有改Cargo配置。VS工具路径运行时发现。完整原始/发布SHA-256、日志结束时刻、转换说明见[24份日志manifest](日志20260927/manifest.json)。早期命令未单独记开始时刻，manifest的raw_finished_at仅文件结束时间，不伪造为完整起止。

| 命令/组合 | 时间与结果 | 日志 |
| --- | --- | --- |
| build.ps1 check + cargo test --locked --test checkpoint | 02:08:57.1499406～02:09:33.5051280+08:00；0/0，11通过 | check-08 / checkpoint-05 / final-timing-01 |
| build.ps1 check + cargo test --locked --lib + cargo clippy --locked --lib --test checkpoint -- -D warnings + cargo fmt --all -- --check | 02:10:05.6272890～02:10:20.5758865+08:00；全0，lib85通过 | check-09 / lib-final / clippy-02 / fmt-final / final-timing-02 |
| cargo test --locked --test workspace_changes --test approval_gate | d859c0a输入，02:04:05结束；0，21+31通过 | legacy-01 |
| cargo test --locked --lib checkpoint_ | d859c0a输入，02:01:32结束；0，5通过，后来已包含lib85回归，不重复计数 | checkpoint-unit-01 |

不重复合计148通过/0失败/0忽略（lib85+checkpoint11+AP31+W21），这是B定向及库回归集合，不是整个workspace全量或最终组合五门禁。最后lib测试出现D `transport_fixture_command`未使用warning，lib运行成功；定向Clippy不包含lib test配置，因此不声称该warning已被B验证消除。经理已安排D修后交C全门禁。

## 失败、磁盘与未执行

- `checkpoint-01`：D盘磁盘满os112，编译失败101，测试未运行。本人本轮incremental删除两次被自动审查拒绝，均未删除；不尝试绕过。经理压缩本轮新缓存保留字节，后确认空间恢复。
- 短暂C盘独立临时target有check-03及checkpoint-02（6通过）原始日志；用户最新要求继续D盘后停止新增C构建，后续全部回D。C证据/产物保留，没有清理旧树。
- `crash-01`：0/3失败，原因是测试夹具两写同路径最后值错误及重开漏显式recover；修夹具后D盘crash-02 3/3，再补newTurn typed断言并被最终lib85覆盖。
- `checkpoint-03`：9/10，129调用单Provider组违反既有64调用上限，尚未进入目标逻辑；改为每组32次真实工具调用，后checkpoint-04 10/10、checkpoint-05 11/11。
- shell有一次误将源码相对路径按仓库根传给子目录git add，失败未暂存；之后正确明确路径提交。最终commit命令末查询不存在cargo进程导致整体shell exit1，Git提交与diff检查实际成功，SHA及干净状态另核。
- 未运行release build、付费live、推送、最终组合五门禁；未替C/D编写其源码。剩余是经理review、D补丁串行整合、C真实传输联测和固定组合验收。

下一步第一操作：保持产品冻结，由经理绑定99ca2b9及本报告审查；如有明确review返工，在原任务/分支追加提交，不amend或重开任务。

## 经理整合后状态补记

本报告完成期间，经理将D最小测试fixture可见性补丁串行合入B，当前工作树HEAD为`dd11094e04af24a52780c83b610422c775c6b525`、status空；B实现受测候选仍是上文`99ca2b9`，未把D补丁后的组合称为B重新验证。经理已继续合到C进行传输定向与最终组合验证。148计数已复核：11+85+31+21，无重复计算内部5项。24份发布hash复核0差异；本人产品与共享档案均停止写入，后续由经理review安排。

## 原始日志临时路径别名说明

manifest中`raw_source`以`.local/temp/`开头的两条原始日志使用逻辑别名：`.local/temp`对应执行进程的`TEMP`环境变量，不表示仓库内存在同名目录。复核时用`Join-Path $env:TEMP 'nhc-b-checkpoint-target/<日志文件名>'`定位原件，不记录本机固定绝对路径。经理已按该映射核对另两份原始SHA-256，24对原始/发布日志全部相符。此次仅补说明，未搬移、复制或删除原始日志，未修改产品或运行构建。
