# B23 98ce MATRIX attempt1：第16例未知停窗

2026-11-02，员工B Jason。按[经理MATRIX唯一签窗](../../审查记录/第二十三轮/2026-11-02B23-98ce构建核收与MATRIX唯一窗口.md)启动，固定98ce / 产品85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e / Rust951cff6854e0a8bc0b7cc48d559b735c60256c21，构建回执SHA `974a1dec4a2087a84c1dfb0cf741f9da2fd8755b581425469a92a42a89940582`。**不是20例通过**。

原exec session `26580` 仍运行，外层supervisor PID `87376` / creation FILETIME `134381028875429933`，内层runner PID `62476` / `134381028898031298`；均在保留unknown路径，未见signal/exit。第16例原test PID `89780` / `134381029069968888`，元数据仍为running、exit null，runner保留其原handle；未见真实signal/exit。内层incident于14:21:47 UTC记录`unknown marker`，外层记录`inner runner reports unknown`，launcher记录`supervisor reports retained unknown`。两gate均存在且被持有，不清不迁、不猜PID结束、不重试；原session未结束。

case16 `breakaway_independent_controls_and_component_denial` 的stderr仅见 `NHC_TERMINAL_UNKNOWN: control helper lacks cleanup receipt`。现场fixture目录有 `allow` 回执 `created=1 query=1 member=0 wait=0`，未见 `deny` 回执；这只能说明允许对照已有其回执，不能推断拒绝对照或进程树已收束，也不能推断未知根因。原case stdout仍停在 `running 1 test`，未有通过结果。第17–20四个state例未启动；case16及后续不计通过。现有控制夹具未把所有在场子进程的原句柄暴露给本报告，不能用PID或文件缺失猜测其生命周期。

前15例的独立case记录均由原handle signal后记录exit0、stdout含 `1 passed; 0 failed`；runner未单独记录各signal时间戳，不能补造。PID/creation FILETIME如下：

| case | 原PID | creation FILETIME |
| --- | ---: | ---: |
| 01 | 80504 | 134381028919773472 |
| 02 | 91392 | 134381028931708154 |
| 03 | 70860 | 134381028939675950 |
| 04 | 59152 | 134381028995675876 |
| 05 | 70148 | 134381029001335390 |
| 06 | 63636 | 134381029006831807 |
| 07 | 80372 | 134381029011741829 |
| 08 | 89844 | 134381029016649964 |
| 09 | 63284 | 134381029021909983 |
| 10 | 91652 | 134381029037378154 |
| 11 | 78624 | 134381029042332654 |
| 12 | 86816 | 134381029048040741 |
| 13 | 91836 | 134381029053134787 |
| 14 | 73868 | 134381029058824917 |
| 15 | 91892 | 134381029064243236 |

按各原测试断言，已过范围包含原Child对象脱stdio后代、超时/取消/Drop、输出上界、Job查询故障、reader已join与仍在途、退出码故障、终止故障/恢复、容量竞争及argv/cwd/env往返；具体例名、原stdout/stderr与测试中原对象/Job/reader断言须看原记录和源码，不能把这些通过扩展为case16或真实C域集成。outer before、before-matrix、unknown三份各196源清单SHA均 `8662e8fc9dae1b58c8ea64e6ed4ebe9a610141c277c787b882cf78579ac5d5ce`；树HEAD保持98ce、未改产品。两锁被占用，常规Get-FileHash无法读取，不能声明其哈希或擅自解除。工件启动前源/副SHA按签窗一致；尚未做矩阵完成后的最终工件核验。

正在保留对象的同时，只读逐源/副双hash发布截至停窗的74份非二进制快照，178184字节，[清单](B23-98ce-MATRIX-attempt1停窗快照/双hash清单.json) SHA `1551eb5703569cb54c69e58729dcab7feb248f3dcc8c48bf4349de192bf89b9a`，复核零差异；不含两个持有中的gate、fixed EXE、临时编译物/target。快照不是最终终态，后续可能产生迟到记录，原现场保留。下一步经理先核原对象及控制夹具现场，裁定未知处置；无自动新窗，也不把既有部分结果当完整矩阵或C验收。

## 经理要求的只读分解与勘误

原`allow`为34字节，原文 `created=1 query=1 member=0 wait=0\n`，SHA256 `eeb0f78804f6805ea438d2029928899de9efeaff5832abb7e62947291309540d`；本次只读仍未见deny。上文“未见deny”是现场事实，并不证明已进入拒绝对照：first allow回执写出后，父侧可能仍在第一调用中因其他条件失败而park；也可能第一调用已返回、第二调用停住。目前没有可区分两者的调用阶段日志。

实际marker位于`breakaway_control`：前一条 `WaitForSingleObject` 必须对该次helper原process返回`WAIT_OBJECT_0` 才进入，而后一次复合条件按短路顺序检查 `GetExitCodeProcess` 失败、非零code、单次Job accounting失败或active非0、receipt.is_file为false；任何一项均打印同一marker并永久park。故可从当前固定源码和marker推知**触发该marker的那次helper原process已signal**；不能从marker断定退出码、Job active、回执判断或哪次调用。`allow`内容由helper成功创建挂起breakaway child后，对该child原对象查成员、TerminateProcess并WaitForSingleObject成功才写临时文件，再rename到目标；receipt记录 `query=1 member=0 wait=0`。此回执不能单独证明park处当前Job已空或当前调用一定为deny。

持有图：本工具exec session26580 → launcher持原supervisor process；supervisor持原runner process；runner持case16原test process；case16在`breakaway_control`永久park栈保留该调用创建的原Job、helper process与thread handle。允许对照可能的breakaway child由helper原handle终止并等待的receipt作有限证据；不可从receipt推断其余Job/调用状态。没有现成外部IPC把park栈中原helper code、原Job accounting或字段分支返回；单靠既有现场日志、文件和原有runner接口，无法在不附加操作下重新查询这些栈内原handle。session当前仍运行，两gate仍持有。

现以`FileAccess.Read`且`FileShare.ReadWrite`兼容持有者取得并只读保全gate字节：common 451字节 SHA `026f1fe719552fe2186bfa74a6660f9ee34e079671c91dd9e39e5990f1073109`；inner 0字节 SHA `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`，[只读副本](B23-98ce-MATRIX-attempt1停窗快照/gate只读副本/)留存。此更正上文普通Get-FileHash不能访问的观察，不改变锁或持有。

建议经理先保留原session与两锁，对照源码和原截图/metadata核以上证据；若要得到迟到原helper/code/Job数据，需先设计并审核能从当前case16持有的**同一原对象**取得可审阅句柄/身份和只读查询结果的方案。现有程序没有此协作通道；仅按PID重开、进程列表、等待TTL或关闭Job都不构成该证明，也无法保证不扰动现场。若不能在保留原对象前提下安全取得，则应如实维持unknown，后续重做受控夹具必须另建候选、保留原事故并经经理裁定窗口，不能反推本次已收束。本轮不执行注入、调试、终止或释放。
