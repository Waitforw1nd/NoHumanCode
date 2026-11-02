# B23 case16 PSS v2诊断副本release结果

2026-11-02，员工B按经理明确指示，在核诊断EXE SHA `815c33b77659215347386f7604a1667fde52b258ee18a6688d239320def43aed`、源码 SHA `1432f90a15b84e0a81ab2068c3f6e72b0fe2d1550e184ed7895b11947bd70209`、原查询stdout SHA `e08d2f5bcc922568cf01de5950bb2bd19b7e4ca86e3bcd0ded48816f67c6711a`及release不存在后，只创建该attempt唯一0字节 `release.request`。

诊断两次重核载体PID89780/creation FILETIME134381029069968888/Wait(0)=258。`release_source` 六个源handle 268、276、280、292、296、6784，验证副本分别264、268、272、276、280、284，全部 `compare_all=1`；18个原查询副本、6个验证副本及诊断自己carrier handle共25个 `release_close` 均 `ok=1 error=0`，随后输出 `RELEASE_COMPLETE`。这只证明诊断代码对自身对象的释放记录，不证明原Job最后关闭或case16收束。

原诊断PID92080/creation FILETIME134381043364666972，异常持有分支记录原handle signal `2026-11-02T14:49:40.6301291Z`、exit0、`release_confirmed=true`；exec session `16881` 实际exit1，原因是之前监控空stdout空值异常，`incident-monitor-exception.json`与`launcher-error.json`原文保留，不改写diagnostic.json先前running。release后完整stdout SHA `accc003e0eccc4c72db4c85224f1c65674683e0607780558ca8f02f4d857708f`。

10份release及查询相关非二进制原件共11014字节已源/副双hash发布，[清单](B23-case16-PSS-v2-release原始证据/双hash清单.json) SHA `dc1365065b68e637118507ff13746b7a0b657c0eb4baa35a60273578f97ad022`；原查询快照单独保留。原矩阵session `26580`仍在、两gate存在；没有终止原case/helper/Job/runner/supervisor、清锁或重跑，case16 unknown不变。
