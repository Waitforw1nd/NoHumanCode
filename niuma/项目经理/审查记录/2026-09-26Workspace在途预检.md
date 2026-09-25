# B-R3-01 在途预检记录

2026-09-26，经理 GPT-6。对象是员工B独立工作树中正在实施的代码，尚无固定候选SHA；下列是提前反馈的实现风险，不是正式交付失败判定或验收结论。最终结论必须在固定SHA逐项复核。

| 编号 | 早检所见风险 | 已交付给B的修法/证据 |
| --- | --- | --- |
| BP01 | prepare/claim/finish事务commit后仍持有db MutexGuard，调用self读取再锁同Mutex | 释放guard或同连接读；真实写入/恢复有界完成 |
| BP02 | restore持gate但perform_tool未协调，且只看自身active，其他活动命令/写可能竞争 | 所有写入口共享协调，活动workspace冲突判定；有屏障竞争测试 |
| BP03 | before解密/摘要在逐文件action才做，第二个坏密文可导致第一文件已写 | 全量预检先解密校验全部before；坏密文零文件写 |
| BP04 | 逐文件仅读leaf，未重新resolve父链 | 副作用前复查root/path/scope/link，外部哨兵不变 |
| BP05 | changes聚合以later finished覆盖restore_state且忽略later unknown | 首次before/最新成功after、恢复历史、unknown可见且不可恢复分别建模 |
| BP06 | 恢复只比record/task，未比当前Host workspace | 当前配置及原审批workspace/scope绑定均复核，配置变化拒绝 |
| BP07 | verify_schema8只有列名/index名称/DDL子串/foreign_key_check | 检查真实列类型/PK/NOT NULL/FK/索引唯一性和列及CHECK；有效夹具与伪装结构反例 |

另提醒：write_file工具description仍称正文保存在event log，应随实现变更改准确说明；Windows规范path key不能仅lowercase字符串而漏别名。上述均在B已授权源码范围内，经理未接管修改。

B已报告标准build.ps1 check通过；其首次未导入MSVC的失败需要原样留证。check通过不证明上述运行时安全。下一操作：接收固定候选并对BP01～07和W01～14核对代码/测试，未关闭项写正式review。
