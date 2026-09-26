# C-R8-01 第0.2次回执与崩溃审查

2026-09-27，经理GPT-6；实现C/GPT-6-astra，D只读交叉审查。固定be51dd5eee655f995d08e559e6e1bca4c257b960。当前未APPROVED；定向失败修复仍在途。

N07-C-D01：cli输出前保存已验证calls，stdout失败附Failure.details，外层不再抹为无ID unknown；真CLI子进程关闭stdout、wait_with_output，断言exit1/output_failed/finished/原receipt及一次POST。此前CLI6/6通过，fixture为脚本化Host，只证明CLI消费和回执保留，不冒称该例证明DB持久化。

Host真崩溃用例先相同Provider完整成功，再第二调用已发且部分正文屏障、SQLite已claimed原ID，真实child.kill并wait，重启同数据目录后原call unknown/success=null，DB仍2条/Provider仍2次。C报告该例实际通过；这覆盖响应未完整，不独立证明完整响应读取后finish事务前窄窗口。后续新增测试须另绑定SHA和日志。

C本次10组定向其余8组通过；approval_http旧夹具4失败、network_http管理撤销预期1失败保留，C继续唯一所有权修复。管理route_models不传旧grant，active为空返回Required合理；明确GET原grant仍revoked且Provider计数不增。不能把所有409都接受。旧审批fixture仅适配新DTO/真实ack/重启相同Provider，保留claim/unknown/恢复次数断言。

经理另读web预览固定revision/digest/purposes确认、显式命令无OS沙箱提示/未默认勾选ack、错误calls展示与本地保存行为；没有409后自动刷新授权。最终仍待固定组合五门禁，原始失败不覆盖。
