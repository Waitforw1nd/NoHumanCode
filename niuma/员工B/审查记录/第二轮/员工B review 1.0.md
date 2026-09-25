# 员工B review 1.0：审批候选

审查日期：2026-09-25；项目经理 GPT-6；受审候选 `50de5456b8c0d412aff4e90821bdde221f066a2d`；依据修订1、修订2及第1.0报告。本轮用户指定GPT-5.6-sol员工并行接续，A独立复核、B实现修复、C适配HTTP旧测试，经理负责裁决。

当前结论：**CHANGES_REQUIRED**。AP12五项旧回归失败已在员工报告如实披露；经理通过[并行收口任务](../../../项目经理/任务/2026-09-25审批候选并行收口.md)有限扩展旧测试适配写权，保留原安全断言。并发现下列公开取消接口问题，需要在原范围修复；其余安全复核进行中。本记录不是最终验收，不冒称经理全仓门禁已通过。

## B-R2-REGRESSION-01：AP12（P1门禁）

第1.0标准test退出101；补跑198通过、5失败、1付费忽略。两个schema6固定预期需要适配7；三项runtime测试需要在真实写工具前显式逐次审批。修法与唯一写入人：B改store原迁移测试及runtime三项，C独立改http_contract schema预期。不能删除/ignore或减少原有依赖环、硬链接、路径、命令权限及HTTP安全头断言。组合后重新运行标准门禁，不用各分支定向通过代替组合全仓通过。

## B-R2-CANCEL-01：持久取消成功后因无active而返回错误（P2，阻断公开API冻结）

A固定候选静态复核提出、经理复查确认：src/engine.rs `Engine::cancel`（候选966附近）先执行Store::cancel_pending_approvals并提交，再查active map；无运行worker返回“成员当前没有运行中的任务”。重启后pending/approved-not_started的取消会改持久审批状态却向调用者报错。现AP18无旧等待者夹具直接调用Store，未覆盖此Engine入口。

具体修法：持久取消成功后，active token可选，存在则cancel，不存在不能仅因此报失败。未知task的存储错误仍保留。已recover为interrupted的task不强制新增Interrupted→Cancelled协议转移，审批取消持久事实与task原终态分开记录；queued/running按已有合法转移写cancelled。不扩大protocol写权。

验证：真正shutdown旧runtime后重开，通过Engine::cancel覆盖pending和approved+not_started；重复取消不新增resolved、原批准决定保留、零execute，resume产生安全取消结果；不能只调用Store或drop Arc。B拥有原engine/approval_gate唯一写权，已向其分发这一具体修复。

## B-R2-REDACT-01：完成结果两条脱敏路径不一致（P1，最小反例已补证）

A与经理静态复核：真实run_command输出 `bearer abc123` 时，finish把JSON结果同时作为tool消息content字符串和结构化tool_result保存。safe_task_value对content进行文本脱敏，现有bearer扫描到空白，会吞掉序列化JSON的尾部；事件递归处理结构化Value则保留对象。Repository finished校验把已不能解析的content作为String与事件对象比较，判CorruptState，可能影响公开查询、恢复乃至Store重开。此处abc123仅为测试标记，没有读取真实凭据。A正在独立构造运行证据，不能把此段静态推导冒充已执行测试。

具体修法交B在原engine/store/approval_gate范围完成：消息和事件使用同一安全表示，且必须经既有safe_task_value再次redact仍稳定。不能只做一次结构化redact后就假定序列化字符串稳定；`bearer [redacted]`仍可能被再次扫描并吞尾。可选保守安全固定文本fallback，但必须保留工具真实成功/错误语义，不能伪造成功、绕过redact或放宽finished证据校验。不得扩写secrets/domain。

验证：真实命令输出mock bearer，审批finished可查询、事件与消息语义一致；真正停止runtime/重开/recover后不重做、数据库不含mock秘密。正常输出、真实错误及既有AP16事务回滚仍须通过。B是唯一实现写入人。

补证结果：A已交[固定SHA独立报告](<../../../员工A/提交报告/第二轮/员工A（审批候选安全复核 第1.0轮报告）.md>)与有限运行日志，候选真实redact函数的两路径探针1/1、退出0，复现消息JSON尾被吞而事件仍为对象。经理已读输出与固定代码链；该探针没有执行外部PowerShell，不冒称完整Engine集成测试，后者由B修复测试覆盖。A未发现其他已形成可复验反例的缺陷，不能因此免除组合门禁。

## 审查范围与后续

经理已静态检查整组预检、definitions/policy/claim顺序、waiter注册后重读、claim前backup边界、finish事务、schema迁移入口及旧失败断言；A继续独立验证结果消息与事件脱敏一致性及约束夹具。未在此时宣布AP01～20全部通过，也不将此前自身以员工B完成的测试当成独立经理复验。

下一步：B交新候选/C交支持候选/A交安全复核后，经理追加具体发现或新增review轮次，固定组合并验证，再决定验收。历史第1.0报告和原候选保留。
