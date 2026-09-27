# 员工C：B 独立 Shell 固定候选只读交叉复核 1.0

日期：2026-09-27。执行人：员工C / GPT-6-astra。任务：C-R13-01，执行ID C-R13-01-20260927-E01；来源为经理同任务附加只读审查派发。

## 结论与范围

固定审查 B 提交 `08f1c0a944b86b583292a7a719b5225cdad43716`，以 `git show`/固定提交 diff 阅读 repository.rs、store.rs、plugin_management.rs、engine.rs 及相关测试。围绕 NEXT-12 S03/S07/S08 未发现生产代码阻断项。本结论是独立静态复核，不替代经理验收或动态测试通过证据，不覆盖之后的 B/D 新提交。

本轮未修改产品，未编译、未运行测试或最终五门禁。C 源码窗口回执为 `ef14f4e1fc76fecad74acf0c5869489fe5bfb7d1`，NoManCode `33211302d2cb5879de8620deafea5d77f5f1f7f2`，Rust `677fc9d6f7c44220dfcd1cc0a7e7d53df432cec8`，当次 status 空、无本人构建。经理随后可在此窗口整合完整候选；不把窗口时刻SHA当作之后工作树现状。

## 核对记录

- S03：Store::claim_approval 在同一 Immediate 事务内读取审批，校验 binding/args/tool/workspace/scopes/allow_commands、Approved/NotStarted、command_network_ack、当前项目权限及对应插件 enabled，再执行条件 Claimed 更新。run_command 检查 builtin.shell，write_file 检查 builtin.files，未见串用。runtime acquire 包含 Store claim，已有有效 lease 不因后续 disable 被追溯取消。
- Unknown：Engine::perform_tool 对 run_command 错误链内私有 CommandOutcomeUnknown 在普通 Err 分支之前返回 ApprovalError::UnknownResult；尚未调用 finish_approval_with_change、未追加 tool_result 或工具消息。调用路径将错误返回；resume 对 Claimed/Unknown 先拒绝，未继续 Provider。此处仅静态核对，之后 B 的真实 crash 补证另由其报告负责。
- S07：schema12 必须已有旧 marker、精确 canonical 表结构、类型/合法值/FK 验证。schema13 使用外层 Immediate；FK 开启，子表先删，临时表保存全部旧字段和 rowid，显式还原，新增 shell enabled=true/revision=0；双向 EXCEPT 验证管理旧数据完全一致。临时清理、新 marker 和 user_version=13 同事务；旧 marker/applied_at 不重写。
- S08：迁移拒绝管理表额外显式 trigger/index、额外 FK 依赖和主库/临时库 staging 冲突。填充/marker失败及回滚测试源码检查保留 FK ON、旧12可验证、临时对象无残留且可重试。13 重开走精确 DDL、两个精确 desired、receipt 类型/范围/递增关系及完整 foreign_key_check；不自动补旧 marker 或缺行。
- 全局 key/CAS：receipt key 保持全局主键，重放比较 plugin_id/enabled/expected_revision，先查旧 receipt，再判当前插件 revision。独立插件 revision 不共享；事务同时保存 desired 与 receipt，失败回滚。旧 key 重放不再次 runtime reconcile。
- 并发 open：preflight 使用快照读取 marker/version；进入最终 Immediate 后重新读取当前 version/两个 marker，apply_schema_13 再读版本，另一 opener 已完成13时严格验证后返回，未沿用过期12判定重复重建。这是静态并发路径判断，未执行双 opener 动态试验。

## 证据边界

repository::shell_migration_tests::historical 从新 Store 建库后重建 canonical12 管理表，不独自代表完整非空业务历史。另已核 engine::n12_true_schema12_migrates_preserving_all_old_rows_and_schema13_reopens：two_write_engine_paths 生成真实非空业务、审批、文件变更、restore、网络状态及加密凭证，随后使用实际 apply_schema_12 重建严格12管理结构；分别验证 files true/false、旧回执重放、13再次重开和旧任务 Sealed。它是当前代码构造的 canonical12 夹具，未运行旧二进制。

该 engine 测试比较所有旧业务表的全部列值；管理两表与 schema_migrations 显式比较 rowid。其 n10_rows 对其他业务表是 SELECT * ORDER BY rowid，不包含隐含 rowid，因此不宣称它动态验证了所有业务表的隐含 rowid；生产迁移没有重建或改写这些业务表。

independent_stores_cas_one_winner 是两个已打开 Store 的 CAS 竞争测试，不是同时 Store::open。源码中可见测试的存在与断言设计不等于本轮运行通过。未发现需要派发返工的稳定问题编号；以上边界已同步经理。

## 下一步

保持产品冻结，由经理完成后续固定候选及交叉审查证据核验。只有收到最终完整SHA/tree明确放行后，C 才执行标准最终五门禁；不得把本次只读复核计为门禁。
