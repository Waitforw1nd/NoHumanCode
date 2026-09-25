# 员工C review 1.1：审批HTTP验收

日期：2026-09-26。经理：GPT-6；实现C及独立审查A均为用户指定gpt-5.6-sol。任务C-R3-01 / NEXT-02D，执行标识C-R3-01-20260926-014759。

结论：**APPROVED（审批HTTP正式契约修订1范围）**。受审最终候选 `00a4c2bd675516ca61fac84c03734052ccb03660`，基线 `379e2aa6378bbe1161c9f65f6655f93876834ac0`。候选提交与验收区分：此前2af3770因AH09/AH11证据缺口未放行；补证提交仅改测试，生产server内容保持不变。A[第1.0审计](<../../../员工A/提交报告/第三轮/员工A（审批HTTP候选安全复核 第1.0轮报告）.md>)与[第1.1差异复核](<../../../员工A/提交报告/第三轮/员工A（审批HTTP候选安全复核 第1.1轮报告）.md>)共同覆盖最终输入。

## 1. 范围与行为

经理核对Git对象、完整server差异及相关测试：相对基线仅src/server.rs、新tests/approval_http.rs两处；diff-check为0，C工作树干净。Rust源码tree为 `869fc18b86ade0bd5d6d18f85b02ecf0400f5011`。Engine/Store/Repository/approval、迁移/依赖/旧测试未变。

三个路由为GET /api/tasks/{id}/approvals、GET /api/approvals/{id}、POST /api/approvals/{id}/decision。返回显式DTO，隐藏内部摘要/参数/工作区/权限；严格body、路径校验、现有guard/token/body limit/安全头保持。决定仅调用Engine公开入口并固定user，返回提交时快照，不启动worker或推断执行完成。

错误按ApprovalError类型链分类，真实缺行404、决定冲突409、损坏/内部错误500；保留SQLite busy/locked cause才可重试。列表通过Store::task先区分不存在与空审批列表，损坏不降为404。未知任务原实现已有安全外层“任务不存在”，补证精确断言通过，无需额外产品改动。

## 2. 两项补证关闭

- C-R3-EVIDENCE-01 / AH09：实际取得两条requested与一条resolved，精确核对事件data字段集合并扫描秘密/正文/arguments；不以空集合或整个任务消息扫描冒充审批事件证据。
- C-R3-EVIDENCE-02 / AH11：逐响应精确code与固定message，兼容error==message、安全头和retryable；在fresh pending上发送413/三guard/malformed/缺CT/token，每次都检查pending、总事件不变、resolved/start/result为0、文件不存在。

AH01～AH14逐项见[员工报告](<../../提交报告/第三轮/员工C（审批HTTP接入 第1.0轮报告）.md>)与A报告。关键真实性经独立静态核查：AH05/06真正shutdown旧runtime；unknown在停止前同时证明claimed、真实命令计数1、零结果提交；finished来自真实HTTP批准和执行；AH13以客户端实际消费游标断流重连Run/Session，非DB最大seq或未读response替代。

## 3. 固定SHA门禁证据

以下由C实际执行；经理读取原始log及带完整SHA的meta并独立汇总计数，A未重复运行。时间为2026-09-26 +08:00。

| 命令 | 开始→结束 | 退出码/结果 |
| --- | --- | --- |
| build.ps1 -Action check | 02:26:47→02:26:48 | 0 |
| build.ps1 -Action test | 02:26:48→02:27:10 | 0；222通过、0失败、1付费live忽略 |
| build.ps1 -Action fmt | 02:27:10→02:27:11 | 0；真实空输出log |
| build.ps1 -Action clippy | 02:27:11→02:27:13 | 0 |
| build.ps1 -Action wasm-check | 02:27:13→02:27:14 | 0 |
| cargo test -p peachsh --locked --test approval_http -- --nocapture | 02:27:31→02:27:38 | 0；14/14 |

全仓构成：lib64、approval_gate31、approval_http14、assessment_adversarial4、final_acceptance9、http_contract11、plugin_catalog17、plugin_host23、runtime10、session_turns17、turn_http17、protocol5，总计222。旧AP网关31项随全量回归通过。证据[目录](../../提交报告/第三轮/C-R3-01证据/)中旧候选运行和一次缺MSVC环境退出101保留，不冒充最终通过。既有proc-macro-error2未来兼容警告非本次失败。未执行付费live或release build。

入库日志按用户已授权的可移植路径规则只替换本机前缀；原始副本保存在忽略的.local，28文件前后hash及10处变更文件见[清单](../../提交报告/第三轮/C-R3-01证据/归档副本前后哈希.json)，原始hash历史保留。路径编辑不重算或伪造运行结果。

## 4. 边界与整合条件

决定入口本身不重算binding；执行前才拒绝绑定变化，测试明确区分。BindingConflict/UnknownResult的mapper为server单测，未伪称decision HTTP可达。部分Repository读取已擦除cause，只能安全500不可重试；本契约明确保留该限制。未知执行不自动重试。本轮未新增UI/CLI、自动恢复API或整个SSE无正文承诺。

允许经理按Git规范保留受审SHA串行合入main，整合后复核Rust源码tree完全相同并另记最终merge SHA。此review不表示推送、发布或整个产品完成；旧工作树和在途档案继续保留。
