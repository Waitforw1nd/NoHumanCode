# full-2 阶段验证独立审查

2026-11-16，D。等最终manifest生成后只读复核，未启动Cargo、产品或真实测试进程。受测HEAD `4b00c481486aa17dd0fc7e0056c334cc64f6c092`，产品tree `ff49de8748250ece88324c20e1b63f8dc996729b`，Rust tree `a092d0e2923d9224850a6dc5f06167d87fef9e47`。

最终status=failed、runner_exit_code=101，执行到第6步失败即停。36项证据一致性检查相符；26件manifest工件和manifest本身共27件文件的SHA256/长度列于 `full-2阶段证据复核.json`，无缺失或额外attempt文件。精确九步计划、argv、步骤started/result/direct-exit、日志hash、串行时间顺序均相符；前后原源码快照字节相同、输入clean、清单摘要及Git双tree相符。失败是测试结果，不是证据一致性失败。

| 门禁顺序 | 实际结果 |
| --- | --- |
| 1 workspace/all-targets check，offline/locked | exit0 |
| 2 debug bins build，offline/locked | exit0 |
| 3 execution_authority_lifecycle 定向 | exit0；18 passed / 0 failed |
| 4 lifecycle_compat 定向 | exit0；4 passed / 0 failed |
| 5 git_diff 定向 | exit0；1 passed / 0 failed |
| 6 build.ps1 test（Rust成功后才网页） | exit101；approval_gate失败，网页未执行 |
| 7 fmt --all -- --check | 未执行 |
| 8 Clippy workspace/all-targets -D warnings | 未执行 |
| 9 UI wasm32-unknown-unknown check | 未执行 |

第6步完整日志75019字节，SHA256 `719c1900c3a5ae0498dd9cd9c275da0c71ae92a47e7b411301ab79d3b48f9db2`。各Rust目标实际汇总：lib 437 passed/0 failed/44 ignored；main 2/0/0；worker 0/0/0；agent_runtime_cli 2/0/0；approval_gate 28/7/0。至停止为止第6步合计469 passed/7 failed/44 ignored，只是已执行目标小计，不是全workspace结果；三项定向18+4+1单列，不重复加入该小计。后续目标和文档测试不能计为已跑。

approval_gate的7项失败全部记录 `data domain controller is busy or unknown`：

- ap06_ap07_ap18_runtime_restart_and_decide_before_waiter（原日志tests/approval_gate.rs:925）
- ap06_reconcile_preserves_order_with_pending_and_unapproved_calls（1645）
- ap08_restart_public_cancel_is_idempotent_without_active_runtime（547）
- ap15_approved_before_claim_stopped_runtime_executes_once（1287）
- ap15_real_command_effect_then_runtime_stop_unknown（1041）
- ap16_finished_restart_never_reexecutes_and_missing_result_is_corrupt（1111）
- redacted_command_result_stays_finished_across_restart（649）

D只报告这些原失败位置，不越界修改approval_gate，不推定可以放开生产controller规则。provider先前六项对应记录已通过：p06两个相位均真实kill/原对象终态后两次恢复，unknown及held_tokens=100保持，mock completion分别保持0和1；这不改变后续approval_gate失败结论。

完整六份原日志和最终manifest按原字节保存在 `full-2原始日志/`。`audit_full_2.py`是本次只读核验脚本，硬编码本候选与明确九步命令，仅调用git读取对象，不调用Cargo或产品；输出文件exclusive create，不覆盖旧报告。

经理已说明另有 `5a9e57a5076b6132c050297b43a0d2f81048963d` 的Engine测试有界shutdown补丁待合，因此本次无论结果如何都只是阶段候选，不能作为最终受测树。此次实际失败更不能引用旧static-2或后续修复补齐剩余三门。最终全套与25矩阵均未在本报告验收；gate保留/进程树清理由经理原监督会话收尾，本审查不以PID快照或直接exit0声称树清理。
