# NEXT-05 联合只读支持：B WIP 50826f4

支持人：员工D / GPT-6-astra；日期：2026-09-27；沿用 D-R3-01-20260927-000811。经理在 D 正式候选/报告冻结后授权只读审查 B 工具会话 WIP；不变更 D 实现候选，不修改 B 源码，不创建测试或运行未授权的额外矩阵。

受审固定提交 `50826f4c8762ff881ea6c0c27a2fc5c834d34045`，差异起点 `2e168cbf2c2b2446aef8eff659047277070ca830`。通过 `git show`/`git diff` 读取固定内容，不把 B 当前在途修改混入结论。本文是静态支持，WIP 未运行测试，不是 B 完整验收。

## 发现 D-N05-01 / P1：中断补齐文本不能使无审批的副作用历史重新可续聊

固定提交 `src/store.rs:1799` 的 `chat_continuation_prefix_tx` 判断：工具需要审批、无对应 approval 时，若 `call.result` 等于 `Execution was interrupted. Inspect files before repeating a write or command.`，则显式放行。

该文本表示缺失执行事实，不能证明工具未产生副作用。存在可由既有代码生成的路径，不需要伪造工具消息：

1. 历史 `0fe73f3` 的 `src/engine.rs:662` 在保存 assistant tool_calls 后直接执行 `workspace::execute`，当时尚无 approvals 网关。一个已有领域映射的工具 Chat 若在 write_file/run_command 副作用已发生、tool_result 尚未保存之间崩溃，会留下未闭合工具调用且无 approval。
2. 升级后 recover 标记 interrupted；用户走既有 resume。固定 `50826f4` 的 `src/engine.rs:727` → `reconcile_open_tools`，`approval_by_tool_call` 返回 None 时，在第740行通过 `record_tool_result` 补入恰好上述文本以及同源 tool_result 事件。
3. 后续 Provider 返回普通回答，旧 Task/Turn 可以 completed。此兼容路径未重新执行旧工具，但不能确认崩溃前副作用的结果。
4. 新 append 的 `src/store.rs:1740` 只扫描审批/change/restore行；该历史无这些记录，SQL 未决条件为 false。`closed_calls` 检查闭合，tool_result事件也由Host真实生成并匹配。
5. 第1756行 `approvals_for_task` 返回空，Repository 的 `validate_approval` 没有记录可检查。第1801行中断字符串例外最终允许新Task，绕过“不明副作用不能被新Task洗掉”的N05边界。

现行版本在“assistant调用已保存、ensure_approval尚未执行”窗口崩溃，也会由同一reconcile路径生成相同字符串。这种现行无副作用情况与历史已产生副作用情况不可仅凭文本区分；因此不能将该文本升级成确定无副作用事实。

最小建议：只在新 `chat_continuation_prefix_tx` 收紧，RequireApproval 的历史新调用若无同源有效审批，返回 `ChatTurnError::UnresolvedEffects`；删除字符串放行例外。保留旧resume的兼容补齐行为，不扩大为重写历史网关。原已提交key仍应优先回放原receipt。

定向验证建议（由B已有N05范围完成）：构造旧工具调用无approval，经真实旧resume补齐文本并完成，再发新key，预期 UnresolvedEffects/零新Turn与Task/零新Provider派发；相同已提交key仍回放。必须明确夹具代表历史未知事实，不把当前版本已知前claim窗口误报为真实写入发生。

## 其他核对与结论边界

- 审批来源：`Repository::approvals_for_task` 调用 `validate_approval`，后者依据 turn_tasks 核对实际turn/session，并对 finished 审批要求唯一 approval_id/call/name/result 事件。因此最初关于“仅查询task_id未校验来源”的候选疑点已撤销，不是返工项。
- 历史闭合：完整工具组、ID唯一、结果对应、前缀继承与新调用ID拒绝重复在固定代码中可见；未发现额外静态阻断，但未替代N03/N06测试。
- 恢复提示：Host固定文本只含 task/restore稳定ID，以 user 角色加入，不把用户内容提升为system；在事务内重新计算候选前缀，已提交key优先回放。本文不证明真实恢复/并发场景通过。
- 未决扫描：遍历Session历史映射Task，覆盖pending、claimed/unknown、prepared/unknown change及partial/unknown restore；发现限定为上述“无审批但中断补齐文本”的显式例外。

实际动作：固定差异与依赖实现只读检索，命令成功；部分路径探测匹配不到预想测试文件，未据其作结论。未修改/测试B工作树。D实现仍 `3d00f121b0c66124cbb2afc2d6fdf0d1cf504c10` 干净冻结；D已交付模块证据不因此失效。经理决定B返工/验收；本文不自动改变B任务状态。
