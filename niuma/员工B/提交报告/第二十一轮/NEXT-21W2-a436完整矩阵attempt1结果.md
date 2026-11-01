# a436 完整矩阵 attempt1 结果

2026-11-02 员工B。按[经理签窗](../../审查记录/第二十一轮/2026-11-02a436矩阵与四门外围复审.md)，在原B树 `a4366b56cc88644336d45f961313f294b05ea5ec`、产品tree `4935f1aa104de548c97a26043cb0d5e442e24dea`、Rust tree `bfea730396003f4aa675cc25dcfaa1c6b39f1e14` 执行唯一完整矩阵。启动前四外围hash及tracked runner/DirectProcess hash符合签窗、clean/index空、无匹配重型进程且共享锁/外围锁均不存在。使用原B缓存、E TMP、offline/locked、jobs2/debug0/incremental0；实际 MSVC 14.43.34808 Hostx86/x64、SDK 10.0.22621.0。

worker/probe build和lib no-run均exit0；list原进程PID 83668、creation FILETIME 134380498947149729、exit0，完整映射21 exact和3 helper。21项各case元数据 passed/exit0，各stdout为1 passed/0 failed，窗口 UTC 2026-11-01 23:37:26.2158212 至23:40:41.9460753；外层工具会话87871退出0，原supervisor PID 81924、creation FILETIME 134380498458152329，23:40:41.9781807 signal/exit0。两锁在成功后不存在。源码192项前后逐项一致，最终clean/index空。每例名称、原进程PID/creation/实际exit及stdout/stderr均在[原始记录](B21-a436-FULL原始证据/attempt/runner/)；经理独立核证见[矩阵与工件核验](../../../项目经理/审查记录/2026-11-02a436矩阵二十一项与工件核验.json)。

三份独立固定件：worker SHA256 `3fd9c6aa65c1f2ed1cb23851e40dbb35da770004b440285aa8ba0c7ee71db572`；probe `20d55aa460182f930a2505b32b05a28b73ca3b98891d114d44eb4ae9d31286fe`；lib-test `ce5d38b905f8bcb4eab6aafe2352361168bb63fea645aff316216f0cc04babe5`。本次与经理分别核3固定件、21例worker及1176份probe共1200个EXE的长度/hash，零差异；工件本体留原现场，未发布EXE。

范围证据：W04忙probe PID32100实采用户CPU 30468750个100ns tick、原等待258、Host预算门赢；W04 final有效响应PID75920的最终采样30781250被拒。W04创建失败分支本次分别真实错误87和5、Job active0，另 `create_returned_suspended` PID82792已回收；不声称本次 `create_after_ticket` 观察到late success。W05 before/suspended/resumed/pending四停点定向结束helper，三份实际worker原观察wait均0，before没有worker；W06事件显式继承正控、非白名单event/Job句柄负控、工件映像与写映射反例、active1和breakaway单因素均按各自实际条件通过；R04真实进程signal/Job active0、无watchdog代劳。具体证据以各case原件为准；用户CPU采样和最终门非硬实时保证，W05/W06结果限定当前测试装配和威胁边界，不代表生产绑定W3。

[发布目录](B21-a436-FULL原始证据/)含窗口与各例所有非EXE、非tmp原件，以及本窗口prepared四件和tracked runner四件，共230件903588字节；复制前源、复制后源与副本SHA256/长度一致，第二次源/副复核零差异。原日志内部路径不改，清单source以仓库根相对定位；[双hash清单](B21-a436-FULL原始证据/双hash清单.json) SHA256 `cb016fccc2a3c530d1d5c993ec974290d7e88e33ee6545ab2c1b6f7b8f3952bb`，[发布与工件复核](B21-a436-FULL原始证据/发布与工件复核.json) SHA256 `136686f05ebea9df777f95d6da653bb8052d172a0cdccd3eae561b333174294a`。不复制EXE/target/tmp，不覆盖旧FULL-C。

当前矩阵已跑且待经理核收发布证据；四门尚未运行，等待经理单独签窗。已通过同a436 check/Clippy仍按前置证据记载，不把它们或此矩阵当作最终W2验收或整合。
