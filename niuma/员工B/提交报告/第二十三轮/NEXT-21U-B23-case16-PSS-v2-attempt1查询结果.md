# B23 case16 PSS v2 attempt1查询结果：诊断对象保持中

2026-11-02，员工B Jason，按[经理只读诊断签窗](../../审查记录/第二十三轮/2026-11-02B23-case16-PSS只读诊断v2签定.md)执行。固定98ce / 产品85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e / Rust951cff6854e0a8bc0b7cc48d559b735c60256c21；启动前五文件、两gate字节、clean/index和新attempt按签窗一致。未修改产品/原脚本、未写release文件。

诊断编译器原PID `90680` / creation FILETIME `134381043357364037`，原handle signal `2026-11-02T14:45:36.4512684Z`、exit0；固定MSVC14.43.34808 Hostx86/x64及SDK10.0.22621.0。实际诊断EXE在员工Rust `.local/b23-case16-pss-diagnostic-v2-attempt1/case16_pss.exe`，189440字节、SHA256 `815c33b77659215347386f7604a1667fde52b258ee18a6688d239320def43aed`，编译源SHA `1432f90a15b84e0a81ab2068c3f6e72b0fe2d1550e184ed7895b11947bd70209`。实际KernelBase路径/解析成功，DLL SHA `0eabe006872acc19c3938db00faf1754ce1e56b02a87a32a1483001d37064676`；cl SHA `5e87e3700437d820002cedf66d44b0db6a0b463d1174a3d8ac3f7fc9efbc5ae0`、link SHA `bb1459293f2001eb536d04c00d2eb96160e69374ac4eb6e1611bf372cd872e1d`、SDK kernel32.Lib SHA `25346e02cffca92abff07000d54e1830fe0d8861c31a114eda472547fe9f2f00`。

诊断原PID `92080` / creation FILETIME `134381043364666972`，exec session `16881` 仍持原process，stdout已输出 `QUERY_COMPLETE`，诊断与18个已复制handle默认保持、未见signal/exit。原矩阵exec session `26580`仍保持；原case16载体每次身份核为PID89780 / FILETIME `134381029069968888` / Wait(0)=258。两gate仍持有且字节与停窗快照一致：common `026f1fe719552fe2186bfa74a6660f9ee34e079671c91dd9e39e5990f1073109`，inner空文件 `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`。

## 全部候选与计数

三次A/B/C每次walk 134项，分别为process 1、thread 5、可识别Job 0、其他128；其中每次29项无类型名。三次累计walk402、process3、thread15、Job0、other384、no-type-name87；各PSS walk结束码259（ERROR_NO_MORE_ITEMS）、marker/snapshot free均0。AB/BC缺失或不匹配0、重复值0、snapshot错误0、duplicate失败0、compare失败0、query失败0、identity不匹配0；共复制18个副本、6个候选通过三副本对象比较。上述0只覆盖可识别和实际查询的候选，不代表其他128项类型/Job已排除。

| 载体handle值 | PSS类型 | 实际副本查询 | 关系/局限 |
| ---: | --- | --- | --- |
| 268 | Thread | TID91472 / PID89780，wait258，snapshot IDs match | carrier自身线程候选 |
| 276 | Thread | TID90256 / PID89780，wait258，snapshot IDs match | carrier自身线程候选 |
| 280 | Thread | TID45204 / PID89780，wait258，snapshot IDs match | carrier自身线程候选 |
| 292 | Thread | TID91164 / PID91236，wait0，snapshot IDs match | 与下方process的PID一致，不凭此认定park栈角色 |
| 296 | Process | PID91236 / creation FILETIME `134381029070411070`，GetProcessTimes成功，wait0，exit查询成功/exit0，snapshot PID match | 候选原对象副本，尚无证据将其唯一绑定到某次control局部变量 |
| 6784 | Thread | TID62416 / PID73644，wait258，snapshot IDs match | 其他线程候选，未确定角色 |

每个候选三次复制的具体本地handle值和原查询输出在原stdout；没有可识别Job，故没有执行成功的Job accounting/ExtendedLimit查询或`IsProcessInJob`配对，也没有Job flags结果。不能把0个可识别Job当作没有Job或Job active=0，不能把某PID的signal/exit0直接当成唯一原helper或整个树终态；PSS/复制不原子、ABA及引用计数边界仍在。

## 实际外围异常与保全

诊断开始时stdout尚空，`launch.ps1:28`对null调用`.Contains`触发异常，已审最外层catch于`2026-11-02T14:45:36.7456583Z`写`incident-monitor-exception.json`并继续持诊断原handle。之后诊断stdout独立可读并已完整输出QUERY_COMPLETE，但`diagnostic.json`仍是running，不能将其改写成正常监控记录；release前/后应按实际异常分支核原对象。此错误未导致诊断原handle释放；未修改已签脚本或重试。原诊断stdout 5022字节、只读共享模式SHA `e08d2f5bcc922568cf01de5950bb2bd19b7e4ca86e3bcd0ded48816f67c6711a`。

已在持有状态只读双hash发布14份非exe/obj/tmp查询快照，共50075字节，[清单](B23-case16-PSS-v2-attempt1查询快照/双hash清单.json) SHA `6b7824e8a496e0c816640deb100168f50535b4122c6517d59501fc722db171a7`；这是查询时快照，不是诊断终态或副本释放证据。下一步经理审全部候选、监控异常和引用边界，另行决定是否授权创建专属release文件；不终止原case/helper/Job/runner/supervisor，不清gate、不重跑矩阵，原unknown不改变。
