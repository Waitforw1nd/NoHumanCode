# B23 case16 NT诊断副本release结果

2026-11-02，员工B按经理明确授权，核EXE `26220ffaf22f2e0927021c899bde9fcfd59acf5ffbba14919d854571f5342e16`、源码 `09f8c7a451015d295c47865f2a707cb9f9a5854bf0b5e1e3ac97a5a7e8375a28`、原查询stdout `dacbdfe66f93fc1d63d0768d631ff59eaea92f57d0de040cb3ebadab759c6736`及release不存在后，只创建该attempt唯一0字节release.request。

载体原identity两次核为89780 / FILETIME134381029069968888 / wait258。源handle284复核副本228、源handle296复核副本232均 `compare_all=1`；六个查询副本204、208、220、212、216、224，两个验证副本228、232，以及诊断自己的carrier handle200，共9条release_close均 `ok=1 error=0`，随后 `RELEASE_COMPLETE`。这不是原Job last-close或case16终态证明。

诊断原PID49760/FILETIME134381054548613346，signal `2026-11-02T15:06:37.1295803Z`、exit0，原exec session1729 exit0；release后stdout SHA `d4c46ae927fb07c4fc469b1a6d2c23ab7d06040ceea79d513db1cdb4dca755e3`。7份非exe/obj/tmp原件7020字节已源/副双hash发布，[清单](B23-case16-NT-release原始证据/双hash清单.json) SHA `3cbb42b77bf92df15722e1e38477e662b7d880459adb042e15ce4316c5fbfa5f`。原矩阵session26580仍在、两gate存在；未终止原case/helper/Job/runner/supervisor，未清gate，case16 unknown保持。
