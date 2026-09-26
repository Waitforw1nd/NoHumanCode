# B-R5-01 API候选review 0.1

2026-09-27，经理GPT-6。固定候选`725f70aecd5568844bc0e650b82d9bb03cfdff6b`，相对完整基线`7a4a001d4296a26086d5196997ae80ac88e1c22b`六个授权源码文件。结论：**公共签名可供C接入；实现仍CHANGES_REQUIRED，非组件/产品验收**。

经理通读checkpoint/domain/lib/repository/store/engine全部固定差异。Checkpoint DTO与三Engine方法符合契约，真实Store事务与共享restore入口已出现，没有用占位成功欺骗C。B报告workspace check exit0；K矩阵和正式证据尚未提交，不认作全量验收。可以作为明确未验收依赖合C以便并行开发。

| 问题 | 影响与具体修改要求 | 验收证据 |
| --- | --- | --- |
| N06-B01（P2） | repository.creation_key全局UNIQUE及Store跨Task key查询把本应Task+创建命令作用域的幂等key升级为全库唯一。移除全局key唯一/跨Task冲突；同Task仍唯一检查点，同key回放/异key冲突 | 两个真实completed Task使用同一合法key各获自己的ID，原Task回放稳定；同Task不同key冲突零新增 |
| N06-B02（P2） | verify_schema_9的normalize删除所有空白，包含SQL字符串字面量，可能将kind='task_ before'这样的错误CHECK认作kind='task_before'。保留字面量的比较或验证实际约束，不以粗略文本归一化掩盖差异 | 精确伪造CHECK字面量含空白、缺失CHECK/FK/UNIQUE分别在open时拒绝，正常新库/连续迁移/重复open仍成功 |

以上已发回原B执行者，继续同任务新提交；K01～K06原矩阵仍须完成，不另开重复实现任务。共享主线没有合入产品；经理未运行产品测试或代写实现。下一步B修复并定向验证，经理协调C合固定API依赖。
