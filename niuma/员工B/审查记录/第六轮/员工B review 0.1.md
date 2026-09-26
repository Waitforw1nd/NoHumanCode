# B-R6-01候选审查0.1（需修，未验收）

2026-09-27经理GPT-6；固定0cce2bd0263803d7b4f21d1c1734961fc37af622的Store/schema/确认初版。DTO前置4b26377契约形状可接入，不代表整体通过。D只读交叉审查，经理实读源码确认：

- N07-B-D01 / P1：repository.rs verify_schema_10的normalize最后全文replacen("ifnotexists","",1)会在引号扫描后修改字面量；伪索引WHERE state='activeifnotexists'可归一化为合法state='active'，却不约束真实active授权。仅删除DDL头完整IF NOT EXISTS前缀；必须真实SQLite伪索引拒绝及active重复行失败证据。
- N07-B-D02 / P2：network_confirm回放只比request_digest后读authorization_id，缺返回grant与请求subject/revision/binding/purposes的关系校验。两个独立FK不保证归属，A confirmation错链B grant仍合法；要求Corrupt拒绝且旧授权决定不变，补真实数据库错链反例。

B已明确接收两项修复；Engine/Task/call/ack仍在实现，不以尚未完成部分伪报已通过。首次cargo check因MSVC link.exe环境失败，未到源码编译；未代为运行产品测试。合D43fc6ee到B36e96a仅开发依赖组合，不是main整合。闭包长单行要求整理可审读。正式验收等待修复候选及真实N01–N10。
