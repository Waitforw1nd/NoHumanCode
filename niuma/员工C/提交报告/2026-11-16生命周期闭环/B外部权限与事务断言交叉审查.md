# B外部权限与事务断言交叉审查

更新人：C；日期：2026-11-16。独立只读审查，产品冻结，本提交仅新增本报告，同时作为本轮独立状态记录。

## 结论

083337ca、e5827b59、96e113c3 未发现阻断项。三者均为经理当前全量运行HEAD 0fb20585的祖先；按文件核对，session_turns.rs相对083337ca、runtime.rs相对e5827b59、execution_authority.rs相对96e113c3均无后续差异。

## 权限与真实命令正控

e5827b59 的 replace_existing_file_preserves_hardlink_and_command_permission 保留原硬链接来源文件不变、目标文件内容及file_backup事件断言。未授权调用仍必须失败，且明确断言“当前任务未授权运行命令”。授权正控改经Engine.start、allow_commands=true、实际pending审批、decide_approval_with_network_ack；仅run_command提供network ack。限定唯一native-command-1工具结果，核验任务completed、实际exit_code=0及stdout包含native-command-ok。没有以“审批Finished”代替命令成功，也没有公开任意进程入口或内部test_scope绕过授权。

## 单controller与事务断言

083337ca 的 runtime::restart_and_http_security 新增活controller下recover失败、任务仍running、事件全值不变断言；随后shutdown settled、drop旧Engine并用Weak确认旧Store释放，再Engine::new_recovered。原恢复数量1、interrupted、事件恢复partial输出及HTTP安全断言保留。

session_turns::t3_same_key_replay_does_not_relaunch_or_grow 保留两个独立Store、std线程Barrier竞争append_chat_turn；保留恰好一个replayed、相同turn/task及表数量断言。应用层新增同域第二Engine必须拒绝，两个并发调用共享一个controller，原唯一创建/重放、同turn与仅一次模型派发断言保留。因此没有把数据库竞争测试全部替换为单Engine串行调用。

session_turns::t9_cancel_restart_and_resume_boundaries 保留取消、partial和已提交未启动三类恢复语义。已停止场景shutdown settled后释放Engine，TempDir单独保留避免销毁数据库。partial场景仍实际drop原runtime、join原线程、检查旧Store Weak销毁后恢复；观察delta改只读SQLite查询，不新增controller。原interrupted、partial内容、route model、重放不派发和无busy断言保留。

## CLI兼容

96e113c3 仅在原file.try_lock失败边界恢复“此数据目录已有一个 🍑sh 实例正在运行”上下文，保留原OS错误链；未吞错误、改锁、放行第二owner或泛化映射其他失败。与C保留的cl08中文、exit1及原Host仍可查询审批断言一致。

## 证据边界与当前状态

上述结论来自固定提交diff及调用上下文静态检查。经理本轮消息报告full3九目标全部通过，并已在0fb20585开始全量；这是经理执行证据，C未运行Cargo、真实进程或任何测试，不将正在运行的全量表述为通过。

本次未修改A/B/D文件、任何产品文件或integration树。当前任务已完成：权限正控与单controller事务断言审查、报告交付。剩余验证由经理当前全量窗口执行；若出现新失败，下一步只读对应精确日志并按重新明确的唯一写权处理。
