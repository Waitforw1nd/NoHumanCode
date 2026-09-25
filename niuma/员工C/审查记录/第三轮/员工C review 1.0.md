# 员工C review 1.0：审批HTTP固定候选补证

日期：2026-09-26；审查人：项目经理GPT-6，独立辅助A为gpt-5.6-sol。任务C-R3-01 / NEXT-02D；候选 `2af377026f83c0b8b3c6f24cf5a268bac07090ee`，开发基线 `379e2aa6378bbe1161c9f65f6655f93876834ac0`。

结论：**CHANGES_REQUIRED（验收证据补强）**。不是审批网关B返工；本次未发现需要修改生产实现的安全缺陷。候选仅server.rs及新增tests/approval_http.rs两处，209行server增改和1762行测试，diff-check通过。C报告定向14/14通过；最终五门禁尚在整理，不以较旧221/1 run作为最终候选证据。

## 阻断完整矩阵验收的两项缺口

| 编号 | 具体证据缺口 | 最小修法与关闭条件 |
| --- | --- | --- |
| C-R3-EVIDENCE-01 / AH09 | 列表DTO已有mock secret/禁止字段扫描；requested/resolved审批事件只检查归属/approval_id，缺敏感内容直接扫描 | 对含mock凭据/正文/原命令的真实pending及decision产生的两类审批事件，检查其安全字段并扫描序列化data；不扫描整个任务事件史，不承诺既有task正文为空 |
| C-R3-EVIDENCE-02 / AH11 | error helper只验证HTTP状态/安全头/error==message/retryable，未验证安全code和固定message；oversize和三guard在已approved卡上请求，不能完整证明保持pending | 为代表性400/403/404/409/413/415/500固定code/message作断言；fresh pending上运行超大body、Host/Origin/sec-fetch-site拒绝，确认状态保持pending、行/事件/副作用不新增 |

经理与A独立核查一致，已交唯一C在原tests/approval_http.rs范围最小补证。不得改B层、旧测试或HTTP全局语义。生产mapper的context/source类型链、显式DTO和三路由静态初审方向正确；AH05/06真实runtime停止与命令副作用窗口已存在，不能因测试通过省略剩余独立核查。

## 后续

保留本候选及实际测试日志，C新增补证提交完整SHA。对最终输入运行相关定向及正式五门禁，记录实际时间/退出码/总数；A对新SHA差异复核，经理另增review 1.1再决定验收/整合。本review不意味着已验收或合并。具体正式范围见[审批HTTP正式契约](../../任务/第三轮/审批HTTP正式契约.md)。
