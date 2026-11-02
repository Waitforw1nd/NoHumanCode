# C35公开commit_turn执行权旁路反馈

2026-11-02，项目经理独立静审，C唯一实现/测试/分支提交，经理review/整合。当前a799在途草稿，未编译/动态。此项是已核源码旁路，不能将相邻已加fence入口作为覆盖证据。[原函数检查点](../../../项目经理/审查记录/2026-11-02公开commit_turn入口静态反例.json)保留当前函数文本、整文件hash与立即重读结果。

## P1：公开回合创建入口未接执行权

src/store.rs的公开Store::commit_turn仍使用db.transaction() → commit_turn_in_tx → tx.commit()，没有execution_ticket_in_tx、execution_fence或commit_execution_tx。PinnedConnection::transaction只是转发SQLite事务，不提供隐式authority校验；commit_turn_in_tx也没有准入/epoch/phase检查，会插入legacy run/tasks并调用Repository::commit_turn创建turn/tasks/dependencies/event/幂等记录。Repository的SAVEPOINT只保证该事务内部回滚，不提供当前controller能力。

相邻commit_turn_bundle_inner已用IMMEDIATE、事务内admission和提交前fence，但它不支配公开commit_turn；后者在http_contract、tool_result_tests以及大量Store单测直接使用。可构造合法图，在controller已关闭、owner epoch已改变或phase错误时仍调用此方法创建queued任务；这是静态可证的未受约束路径，尚未动态复现。

C应修公开入口本身，保持正常创建/幂等/原子回滚行为，并对关闭controller、旧epoch、错误phase、提交确认丢失提供可判别负控及正常正控。测试须真正调用该公开方法、验证没有新增turn/legacy task/event/幂等副产物；提交已发生但确认不明分支则应验证已存事实并封门，不谎称回滚。不得删除公开方法、只迁测试到bundle或用测试专用无校验票绕过。原测试可由真实Store/Engine生命周期夹具适配，保留原业务断言。

这也是完整入口审计映射仍未完成的证据：后续继续按每个公开创建/claim/迟到结果/运行状态/恢复入口逐个映射到底层准入和提交fence，不能仅计新增测试数。所有原草稿/历史证据保留，未开放动态窗口，不合主线半成品。
