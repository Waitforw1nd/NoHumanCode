# B-R5-01 Checkpoint与Engine review 1.0

2026-09-27，经理GPT-6。结论：**限定组件范围APPROVED**；NEXT-06还须C固定组合五门禁与联合验收，不把组件通过当主线已实现。

B受测实现`99ca2b9e9f2233b3556aec32ab69e3754ad196b7`，Rust tree `c642a7896079736967103ca367019b148476e709`。经理已通读初始生产差异、d859c0a修复及最终Engine接入，核主要K矩阵、真实gate Pending和三个kill/wait测试代码。D对39d0bbde的[只读交叉审查](../../../员工D/提交报告/第四轮/NEXT06只读交叉审查-B-39d0bbde.md)在manifest、幂等、claim、unknown、schema9范围未发现新阻断；D没有代跑B测试。

review0.1的N06-B01已关闭：creation_key不再全局UNIQUE，同Task依旧唯一，同key回放、异keyConflict；两个真实Task同key生成不同ID的测试通过。N06-B02已关闭：DDL比较保留引号内字面量，伪kind空白、缺CHECK/UNIQUE及schema9缺marker拒绝，新库/迁移/重复open通过。完整DDL精确比较还覆盖FK形状，不能仅以表存在认可schema。

创建先在同Host gate内验证completed、根/活动、批准与完成事实、before密文和摘要、最后after及上限，再在immediate事务重核Task与完整有序源集合，和checkpoint.created一次提交。不可变manifest含源身份摘要及最早before/最后after条目，正文/密文不出DTO；task_before并非创建时目录快照。创建后旧Task prepare/resume拒绝。

两恢复入口都核已有checkpoint，复用原restore_locked、claim/outcome和回执；complete回放不覆盖外改，partial/unknown不重新执行，新Turn继续受未决副作用阻断。真崩溃夹具由父进程kill并wait，子进程分别停在claim前/后/文件效果后，重开显式recover；不是drop Arc。Git Diff入口同gate绑定Task和当前Host工作区，changes为空仍读取真实Git三视图。

经理核24对原始/发布日志SHA-256全部相符；其中两份历史C盘日志按运行时TEMP别名解析，未删除或转写原件。实际定向集合148通过=lib85+checkpoint11+AP31+W21，内部5项包含在lib中未重复计数；check、定向Clippy、fmt均0。分候选/失败/环境说明见[正式报告](<../../提交报告/第五轮/员工B（Checkpoint与Engine接入 第1.0轮报告）.md>)，经理本轮没有运行产品测试。

验收边界：K01～K06由B组件证据及随后C新入口联测共同闭合；schema8→9有真实全部业务列/密文保全，schema6/7结构路径仅config哨兵加既有迁移回归，不冒称分别重建全业务图；B partial是确定性单路径故障注入，非磁盘硬件故障。D cfg(test) warning已由D最小补丁1b80486处理，最终组合全目标检查由C负责。单Host共享Engine、普通文件字节/存在性范围保持，未扩目录、命令回滚或多Host并发保证。

经理已合D补丁至B dd11094、再合C39d0bbde；C定向HTTP3/CLI6及映射2通过，最后夹具补充固定296dfea，五门禁尚待。B产品与档案冻结，保持原执行ID，不再创建重复任务；主线未合NEXT-06产品。
