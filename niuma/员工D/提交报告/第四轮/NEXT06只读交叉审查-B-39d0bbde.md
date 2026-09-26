# NEXT-06 B Checkpoint/Engine 只读交叉审查

2026-09-27，员工D / GPT-6-astra，沿 `D-R4-01-20260927-N06` 执行经理附加支持任务。固定审查对象 **`39d0bbdeb8c6cfde028ce14799460696fdf8c562`**，Rust tree `9d74e444f44b13d9007e956d7f2d1cea26d5239c`；不是跟随C工作树的在途修改。以 `git show`/固定commit diff读取源码，未修改产品、运行构建/测试、写共享index或派新代理。

结论：在本次指定的manifest冻结、Task+key、两恢复入口同claim、unknown不重试及schema9验证范围，**未发现新的阻断问题**。这是固定代码及测试实现的静态交叉复核，不是D实跑B测试，也不替代经理review、C最终组合五门禁或完整NEXT-06验收。

## 逐项核对

以下行号均属于固定39d0bbde，文件相对 `NoManCode/rust-app/`。

| 重点 | 固定代码证据 | 复核结论 |
| --- | --- | --- |
| 源事实冻结 | `checkpoint.rs::source_digest`；`store.rs:3318`、`:3325`、`:3350` | 按created_at,rowid读取全部有序源记录，摘要覆盖change ID、Task/tool call、workspace/binding、scope、path/key、kind、before密文身份及前后digest/state。manifest保存Task/workspace/scopes、源集合摘要和路径条目；查询重算源集合、按首次before/最后after重组条目并比较，不用新源覆盖旧manifest |
| before可信/有界 | `engine.rs:2935`、`:3039` | completed、当前Host根和活跃Task预检；所有finished before逐项解密/核hash，包括中间写入；超过8MiB/128路径拒绝；复用validated_restore_changes核真实write_file调用、批准且Finished的审批、binding、args和after摘要；当前文件逐项匹配最后after才创建 |
| 创建事务/幂等 | `store.rs:3255`；`repository.rs:2519` | immediate事务重核Task safe value/completed、源集合、pending/claimed/unknown/restore状态。checkpoint行与唯一安全创建事件同事务；Task唯一，creation_key不是全局唯一，故不同Task可复用key。同Task同key回放验证后的原ID，异keyConflict；恢复后同key仍回放 |
| 旧Task封存 | `store.rs:261`、`:916` | resume_task在写Task/事件前检查checkpoint；prepare_workspace_change在写文件准备事实前同时检查restore/checkpoint。新Turn另建Task，不通过续写旧Task扩展manifest。完整新Turn流程仍由C联测 |
| 同一恢复claim | `engine.rs:1417`、`:3072`；`store.rs:994` | Task restore和checkpoint restore均同gate，内部restore_locked不递归锁。两入口都校验已有checkpoint，兼容Task入口不能绕损坏manifest。全部路径先预检，再经同一个claim_restore immediate事务领取原workspace_restore，不建立第二套恢复事实 |
| 回执/unknown | `engine.rs:1417`；`store.rs:1433` | complete回执先于文件恢复返回，不覆盖完成后再次外改；claimed/unknown返回typed Unknown，partial返回带receipt的Conflict。recover将claimed restore/outcomes置unknown；恢复路径无自动重新claim/重试。source_digest有意不含restore_state，使合法恢复进度变化不破坏checkpoint身份，但checkpoint查询仍校验其枚举值 |
| schema9 | `repository.rs:2530`、`:2540`；`store.rs:46`、`:141` | schema8→9的DDL、marker与user_version同事务；重复open核验完整DDL（只忽略引号外空白/大小写，保留字符串字面量）及FK检查，检查Task UNIQUE/FK、kind/generation等约束；user_version9缺marker先拒绝，不静默补表修复伪schema9 |
| Git Engine衔接 | `engine.rs:2880`后git_diff实现 | 同gate下检查Task、当前Host根、同根活跃Task，然后调用D真实Git读取；不从changes拼正文。实际CLI/HTTP映射不在本次审查范围 |

## 测试代码与证据边界

已只读核对 `tests/checkpoint.rs` 的K01～K05夹具：真实批准write的修改/新建/同路径两写、重开Store、相同receipt、完成后外改不覆写；同key并发/不同Task同key；INSERT触发器失败零checkpoint及事件；外改、before密文、manifest、源记录和scope损坏；超128路径/8MiB；schema6/7/8→9、新库重复open及伪约束/marker拒绝。

`engine.rs:2329`的三个checkpoint真实崩溃夹具在checkpoint新入口进入claim前、claim后文件前、文件效果后outcome前停点；parent读取数据库/磁盘、kill并wait，再重开Store并显式recover。已claim两类断言unknown及同checkpoint身份、磁盘字节保持、旧resume与新Turn拒绝；claim前无claim允许显式再恢复。`:2473`共享gate夹具先poll Pending，`:2523`第二路径故障产生partial并验证同回执/禁止新Turn。这里确认的是代码测试方法和断言有效性，D没有重新运行这些测试。

查阅[B第1.0轮报告](../../../员工B/提交报告/第五轮/员工B（Checkpoint与Engine接入%20第1.0轮报告）.md)：B声明的148项为lib85+checkpoint11+AP31+W21，内部5项未重复计数；C临时target、磁盘/夹具失败及D cfg(test) warning边界均保留。该计数仅引用B报告，不是D新增实跑结果。

保留边界：完整旧业务逐值/密文保全夹具专门针对schema8→9；schema6/7路径在本文件中是降版结构加config哨兵，不应把它描述为分别对6/7每一列完整业务图另作一次同等实跑。partial使用确定性执行前注入，并非真实磁盘硬件失败；HTTP/CLI、鉴权/SSE、真正Host层重启和最终固定组合五门禁继续由C承担。单Host共享Engine/gate的既有范围保持，不扩称多进程共同写同一DB的并发证明。

## 交接

D产品源码仍冻结在 `1b80486f682d978162e056d4a8605f4074187c41`，本次未变更。经理可以结合本只读结论继续审查固定组合；若C或经理后续发现实际反例，再按原唯一写入人修订。D下一操作为保持源码及本轮档案冻结，等待明确review支持。
