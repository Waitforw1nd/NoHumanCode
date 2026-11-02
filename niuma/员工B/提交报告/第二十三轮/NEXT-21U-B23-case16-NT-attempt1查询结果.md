# B23 case16 NT单进程句柄表 attempt1查询结果

2026-11-02，员工B Jason。按[经理签窗](../../审查记录/第二十三轮/2026-11-02B23-PSS释放核收与NT句柄诊断签定.md)对原case16载体执行一次限定诊断，原产品98ce / 产品85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e / Rust951cff6854e0a8bc0b7cc48d559b735c60256c21，启动前后工作树clean/index空。原矩阵exec26580及两gate未动；本诊断exec `1729` 仍运行、持有诊断原handle及6个目标对象副本，未写release.request。

编译器原PID `46880` / creation FILETIME `134381054541708708`，signal `2026-11-02T15:04:14.8448292Z`、exit0；诊断原PID `49760` / `134381054548613346`，`QUERY_COMPLETE` 记录 `2026-11-02T15:04:15.3420420Z`，状态 `query_complete_waiting_release`，未signal/exit。EXE 197120字节 SHA `26220ffaf22f2e0927021c899bde9fcfd59acf5ffbba14919d854571f5342e16`，编译源 `09f8c7a451015d295c47865f2a707cb9f9a5854bf0b5e1e3ac97a5a7e8375a28`；cl `5e87e3700437d820002cedf66d44b0db6a0b463d1174a3d8ac3f7fc9efbc5ae0`、link `bb1459293f2001eb536d04c00d2eb96160e69374ac4eb6e1611bf372cd872e1d`、SDK kernel32.Lib `25346e02cffca92abff07000d54e1830fe0d8861c31a114eda472547fe9f2f00`、实际系统KernelBase.dll `0eabe006872acc19c3938db00faf1754ce1e56b02a87a32a1483001d37064676`、ntdll.dll `6d4730b4553dc7912b5e2988222c6605991b72b4046ff6214355d4386097a488`。编译/诊断stderr为空。

## 实际查询

诊断自身两个空Job和自身Process经NtQueryObject类型名核验均match；自身句柄表得到本次Job index **6**、Process index **7**，仅此本机本次观察，不是固定类型号或公开稳定契约。载体每次核PID89780 / creation `134381029069968888` / Wait(0)=258。A/B/C三次各134项，按现场校准索引每次筛得1 Job、1 Process、其他132项；三次累计walk402、Job3、Process3、其他396。`thread=0`与`no_type_name=0`是本工具未筛选该类别/未采集PSS类型名，**不代表载体没有线程或未知类型**。

| 载体源handle | 实际本地三份副本 | 本次结果 |
| ---: | --- | --- |
| 284 | 204、208、220 | NtQueryObject实际类型Job match；三副本同对象；basic accounting成功，active=0、total=2；extended limits成功，原flags `0x00002800`，breakaway=1、silent-breakaway=0、kill-on-close=1 |
| 296 | 212、216、224 | NtQueryObject实际类型Process match；三副本同对象；PID91236 / creation `134381029070411070`，GetProcessTimes成功、wait0、exit查询成功/exit0 |

对上述两个副本的 `IsProcessInJob` 返回success、`in_job=1`。各三次表采样和查询NTSTATUS均0；snapshot错误、AB/BC缺失或不匹配、duplicate/compare/type-query/身份匹配失败计数均0；目标副本6、验证候选2。stdout SHA `dacbdfe66f93fc1d63d0768d631ff59eaea92f57d0de040cb3ebadab759c6736`，原stdout含全部逐项记录。

Job flags与原允许breakaway控制配置相符，但单进程句柄表/索引/复制关系仍不原子，不能仅由这些数据证明该Job必为park栈局部变量、进程必为特定调用helper、active0等价树/I/O全部完成，或推断last-close；观察到的process已signal/exit0及in_job是这些候选副本的事实。原case16 marker仍是复合条件未知，事故未解除。

只读双hash发布查询时13份非exe/obj/tmp快照、49178字节，[清单](B23-case16-NT-attempt1查询快照/双hash清单.json) SHA `ae095e1f429625429d0404d1b1af9cb0ed2b539828c3215c7c5d1a396ec9d7bb`；查询stdout及准备包等原件留存。两gate原字节SHA分别 `026f1fe719552fe2186bfa74a6660f9ee34e079671c91dd9e39e5990f1073109`、`e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`，仍被持有。未执行产品测试、未终止、未释放诊断副本、不重跑；下一步经理审实际对象关系与引用风险，另行裁定诊断release。
