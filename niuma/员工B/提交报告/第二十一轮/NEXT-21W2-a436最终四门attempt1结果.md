# a436 最终四门 attempt1 结果

2026-11-02 员工B。按[经理签窗](../../审查记录/第二十一轮/2026-11-02a436矩阵核收与最终四门签窗.md)，原B树固定 HEAD `a4366b56cc88644336d45f961313f294b05ea5ec`、产品tree `4935f1aa104de548c97a26043cb0d5e442e24dea`、Rust tree `bfea730396003f4aa675cc25dcfaa1c6b39f1e14`，启动前 clean/index空、四外围hash吻合、无匹配重型进程、两锁不存在。a436前置check/Clippy已有各exit0证据，本窗不重复。

原工具会话36068 exit0；supervisor原句柄 PID 80576、creation FILETIME 134380505567753055，UTC 2026-11-02 00:01:08.9610075 signal/exit0，window-result completed/0。四门按序为：

| 门禁 | 开始 UTC | 结束 UTC | PID / creation FILETIME | exit |
| --- | --- | --- | --- | --- |
| debug Host build | 11-01 23:49:18.7209829 | 11-01 23:49:41.0042805 | 83420 / 134380505587320053 | 0 |
| workspace test `--test-threads=1` | 11-01 23:49:41.8126728 | 11-02 00:00:34.1440261 | 83644 / 134380505818144473 | 0 |
| fmt check | 11-02 00:00:34.8763348 | 11-02 00:00:36.4376761 | 75172 / 134380512348781143 | 0 |
| UI wasm check | 11-02 00:00:37.1271581 | 11-02 00:01:08.1530533 | 52764 / 134380512371288528 | 0 |

完整命令和原进程证据在各步JSON；workspace输出64个 `test result` 块，合计849 passed、0 failed、25 ignored，全部状态ok，逐块摘要及原stdout行号见[测试复核](B21-a436最终四门原始证据/发布源码工件与测试复核.json)。本结果仅为显式单线程，不称默认并发测试；ignored用例并未因本窗执行。UI wasm stderr有依赖 `proc-macro-error2` 未来不兼容提示，实际exit0。环境记录已核MSVC14.43.34808 Hostx86/x64、SDK10.0.22621.0，原B缓存、E TMP、jobs2/debug0/incremental0、offline/locked（fmt为原命令）。

十份窗口源码快照各192项、逐项长度及SHA256全同；窗口后当前192文件再次核hash/长度零差异，clean/index空。Host build时源与新独立副本 SHA256 `c891c9f09ed0e2842268fe2b7de9695ac5b76dbdb56cae6c3dc8479f85ccbebb`，末次独立副本再次核同；末次可变target Host为 `4e6d8dd555b848cd0f1b0518adea8a68df7256ee0a95b0238195fd5d9efd546c`，仅记现场值，不作为独立副本或产品变化的证据。两锁成功后不存在。

[原始证据](B21-a436最终四门原始证据/)含现场顶层41件及prepared四件，共45件、463452字节；源复制前/后与副本双hash和长度一致，再次源/副核对零差异；不复制EXE、target、tmp，原日志内部路径不改。[清单](B21-a436最终四门原始证据/双hash清单.json) SHA256 `f565cd18a6ba1a49a1e166b94994d68f11c2af42a91c9d24f4f0b71aca8ccb7d`；[发布、源码、工件与测试复核](B21-a436最终四门原始证据/发布源码工件与测试复核.json) SHA256 `07c4e12271ca85facae4628cf41662dcd9e1b50792a982f6ffaf229abd6937d7`。初次只读当前源码复核的路径基点误指向源码目录，多拼一层 `NoManCode`，导致192项未找到；已在同一派生复核中改为独立工作树Git根并重算，192项零差异，原记录及清单未变。

本次四门和同a436前置check/Clippy、完整21矩阵已形成候选证据，交经理独立核验及决定W2验收/整合；不自行宣布W2完成。生产工件绑定/W3、统一执行权U及项目后续范围不由本轮证明。

## 经理核验后的发布复核

经理已核四门原进程与supervisor终态、64块849/0/25、十份192源码快照及当前源码和Host独立副本，见[经理实际核验](../../../项目经理/审查记录/2026-11-02a436最终四门实际核验.json)。员工再核45份、463452字节源/副/清单hash和长度零差异，清单及复核hash不变；Host独立副本仍为 `c891c9f09ed0e2842268fe2b7de9695ac5b76dbdb56cae6c3dc8479f85ccbebb`。可变target末次 `4e6d8dd555b848cd0f1b0518adea8a68df7256ee0a95b0238195fd5d9efd546c` 为记录的末次现场值，构建/测试可改变target，不能称为固定工件。a436三SHA、clean/index及两锁不存在状态保持；未再改产品或运行重型。等待经理W2组件验收/整合，生产配对/绑定/W3继续开放。
