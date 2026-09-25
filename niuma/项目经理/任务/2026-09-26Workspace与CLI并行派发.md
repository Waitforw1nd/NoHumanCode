# NEXT-03A / NEXT-04A 并行派发

派发日期：2026-09-26；经理 GPT-6，员工 B/C/A 均沿用用户指定 gpt-5.6-sol。来源：用户在 NEXT-02D 结项后连续指令“继续”。本轮只读预检完成后，经理于本地 02:45 后冻结契约并创建工作树；员工记录各自真实接收时刻。新任务不替代或重开已验收 B-R2/C-R3。

## 现状和继承

完整源码基线 `e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`，Rust tree `869fc18b86ade0bd5d6d18f85b02ecf0400f5011`。共享main源码干净、index空。继承未提交三份修改：B第二轮员工提示词、审批与Capability网关契约、经理2026-09-25NEXT-02C审批网关任务包；六份未跟踪历史档案：B修订2提示词/契约、C审批HTTP草案、经理审批契约接续与HTTP草案任务、NEXT-02C契约补充审查、接任与审批契约收口报告。全部保留，不混入本轮归档。

旧工作树 nhc-b-approval、nhc-c-approval-http、nhc-c-approval-regression 保留。未推送、未发布。

## 分工

| 员工 | 正式任务 | 工作树与分支 | 唯一写权 |
| --- | --- | --- | --- |
| B | B-R3-01 / NEXT-03A | `../nhc-b-workspace/` / `codex/b/workspace` | [文件变更与安全恢复契约](../../员工B/任务/第三轮/文件变更与安全恢复契约.md)列出的后端/HTTP/新测试及有限旧断言适配 |
| C | C-R4-01 / NEXT-04A | `../nhc-c-cli/` / `codex/c/approval-cli` | [审批CLI契约](../../员工C/任务/第四轮/审批CLI契约.md)的main、新cli、新cli测试三处 |
| A | A-NEXT03-04-AUDIT | 共享只读基线及固定候选 | 仅本人第四轮独立审查报告和身份，无产品写权 |
| 经理 | 契约、审查、组合与整合 | 共享main及必要组合工作树 | 经理档案/任务契约/共享index，不接管产品实现 |

独立分支由对应员工提交限定源码；经理审查固定SHA后串行组合门禁与整合。人员档案一律共享niuma本人目录。标准check/test/fmt/clippy/wasm-check，不发布build或付费live。输出独立target，保留真实失败/未运行与原始日志。

## 决策

Workspace第一切片只覆盖 write_file 的任务前镜像、最新成功产物、冲突安全恢复、schema8/typed API；原changes正文改安全元数据，是明确兼容变化。旧脱敏file_backup不能用于精确恢复。多文件恢复明确partial/unknown，不能声称文件系统事务。任意命令副作用、完整Git diff/目录checkpoint及接受流程后续另派。

CLI首片只接已验收审批HTTP，使用固定loopback origin和port，禁止代理/重定向；在所有Store/目录/锁行为之前分派。新的Workspace API未冻结实现前不接入旧不安全restore；完整create/send/events/resume后续另派。

下一操作：接收A契约审查，必要新增可追溯修订；接收员工固定实现候选，逐项复核W/CL证据与实际API，阻断闭合后组合验证。当前未验收本轮实现。

## 2026-09-26 接续与支持分工

CLI 最终候选 0d4dc9af863458e3d6fe2ec7c8b380eebaaafdcc 已按范围验收并经 43dea036536f43d7834ab65e2fa64af231df6204 合 main，旧 C/A 的模型仍为 gpt-5.6-sol。B 原服务余额不足 403 中断，六处在途修改已按字节保全；用户明确允许员工改用 GPT-6-sol。B-R3-01-20260926-024830 由新 GPT-6-sol 员工原位唯一接续，基线/分支/任务范围不变。

新增 C-R5-CRASH-SUPPORT，GPT-6-sol，详见[支持契约](../../员工C/任务/第五轮/Workspace真实崩溃补证支持.md)：基于 dd57adfc9321454651bdcdabb3a081bc95091600，在 ../nhc-c-workspace-crash、codex/c/workspace-crash-tests 独立仅新增 tests/workspace_crash.rs。员工 C 提交自己分支、经理串行整合；B 不写该文件，C 不写 B 原范围。B 在原 engine.rs 内可追加仅 cfg(test) 私有 phase barrier 与真正停止执行者的单元测试来确定性覆盖副作用前窗口；不得新增生产 hook、公开控制接口、Cargo feature 或运行时环境后门。C 负责后副作用/提交后和真实 HTTP 崩溃场景。所有支持结果仍需固定组合证据，不能宣称 Workspace 已验收。