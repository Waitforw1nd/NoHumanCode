# NEXT-18 最终 attempt1 失败审查

2026-10-31，项目经理。结论：完整组合 CHANGES_REQUIRED；C已通过的公开组件范围不因此撤销，NEXT-18不得整合main。

## 固定版本与实际结果

候选 `7fce5b2e187a1147f4fcc784b5d07580eb6afe2c`，NoManCode tree `da5787dfff470eda857fff8c7934432ba5fc8cc3`，Rust tree `b5846dcd4c4a8584d2b36fe0a2c95e688c2fbf83`。原件位于相邻C工作树 `.local/logs/next18-final-attempt1/`。

显式CLI build、workspace all-targets check均exit0；workspace test exit101，首失败停止；fmt、all-targets Clippy、WASM未执行。经理独立核6份同hash源码清单、168个当前输入与Git检出一致，其中164个直接匹配blob、4个cmd按gitattributes检出CRLF。源码未变。

经理从原始test.log重算49个结果块，停止前合计741 passed / 1 failed / 1 ignored，含辅助入口；这是未完成workspace结果，不能与组件数累加或写成完整验收。lib314/0/0，first_user_workflow1/0/0，新runtime CLI6/0/0、schema7/0/0、workflow7/0/0；workspace_crash5/1/0。

## N18-F01：文件写入崩溃测试未到达目标边界

失败入口为 `NoManCode/rust-app/tests/workspace_crash.rs:567`，用例 `write_crash_after_file_before_finish_commit_is_unknown`。实际文件仍为 `before-crash`，日志 `state=Some("failed")` 来自 `workspace_changes.state`，不是Task.status。C封存报告确认日志未打印root/task ID，按独立TMP中双夹具标记搜索未发现可归属目录；Task实际终态及错误、审批/调用明细无法还原，不能从该日志推断Task已失败。

测试要求实际文件已修改、变更记录仍prepared且SQLite写事务正在进行，之后真实kill/wait并验证恢复为unknown。当前失败发生在到达该边界之前，故不证明目标恢复行为失败，也不证明产品没有问题。不能直接判为偶发、超时或夹具错误。另一个完成提交后的写入崩溃用例及两项恢复用例通过。

经理只读追到Engine的prepare/recheck、tool_start、工具执行失败及finish路径；多处可将变更记录标为failed，仅凭现有输出不能归因。D获独立诊断，产品修复必须交实际owner；不得延长timeout、降低并发、删断言或用重复通过掩盖首失败。

## 证据核验与边界

经理独立核36个保全目录的538个原件/副本文件对，包括W08三现场、runtime十三现场、A10五现场及十五阶段快照；按原日志显式a10-fixture另映射五个现场阶段。未打开原始数据库。runtime十三现场另按PID、kill/wait、两次新进程重开及queryable只读快照核验，234文件的阶段和持久语义符合已完成库测试要求。

核验见经理[最终attempt1原件核验](../../../项目经理/审查记录/2026-10-31NEXT-18-C最终attempt1原件核验.json)及[最终runtime现场核验](../../../项目经理/审查记录/2026-10-31NEXT-18-C最终runtime现场核验.json)。这些成功现场不替代失败workspace现场或剩余门禁。C封存后经理另核14对原/发布文本及5份纯路径转换；v2 manifest SHA256为 `7559B7BE7228D55A47E0EA7AEE6C48A2D1521536C4BA726013B4AD682FD6A3BB`，v1索引与清单保留。C失败报告和封存接受，不构成功能验收。

## 后续

C已封存、结束并释放窗口；B/C保持源码与暂存区冻结。经理已将完整7fce5b2快进到原D树，D沿原任务独立诊断N18-F01，新增唯一写入边界另见经理任务补充。修复固定SHA、静态审查和针对性动态验证完成后，再由C执行完整五门禁。main产品仍为已验收NEXT-17；本轮经理未修改产品或执行产品测试。
