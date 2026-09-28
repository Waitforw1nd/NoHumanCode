# NEXT-14预算持久组件检查点0.1

2026-09-28，员工B/GPT-6-astra，B-R13-01-20260928-E01。基线3a94ff41b7fb44e6eae225e6d8934b081e1ae8b4，独立树../nhc-b-budget-ledger。已完整读v1.1/补充01、本人任务、PRE审查，正式真实接收已登记；补充02不改变B DTO。此为中间检查点，不是功能完成或最终验收。

当前固定0ec607f7739d28937d66c93de1a7039813d92884 / NoManCodeec34d46912804eaf446ebaae0de7a6239037721a / Rust2700b27154614eb919a0da9ca19ed6632ae6b864。此前edf8613公共DTO经经理合D6ff真实classifier后为753f52c；c29fdc2首ledger实现；4e9b3ed校验修订及8组组件；0ec只改测试错误提取，避免要求含credential的ProviderPermit实现Debug。

已实现待完整验证：schema15三个追加表、Project固定coverage rowid边界、配置CAS/global key回放、逐call预约/结算/恢复；network claim同事务预算占用、finish_completion分类用量并同事务终态保存、原Models/Balance保持；Engine四同步公开方法真实转发。Task usage只从Valid三整数checked投影，overflow不回滚已提交账本。永久unknown/overrun阻新Completion，历史unknown明确unmanaged不倒填。配置/账项/网络JSON严格交叉验证，首次持久结果回放。

经理问题进展：N14-B01已核SQL budget_requests.project_id和JSON scope、policy当前值/完整revision历史及账项R快照；N14-B02冲突返回首次真实网络receipt及真实dispatch，不回未提交candidate，corrupt未知无candidate；N14-B03所有预算诊断/finish/recover校验配对，network已terminal预算claimed或生效后缺账拒绝。三者已有本检查点SQL/收尾反例通过，仍待经理独立复审。

C依赖：Engine budget_config/configure_budget/budget_status/budget_call全部同步Result<T,BudgetError>；BudgetError.pub kind和kind.code；NetworkErrorKind新增Budget*与budget_code；cfg(test) engine::crash_probe::arm为pub(crate)。Completion专用三phase精确为budget-before-claim、budget-after-claim、budget-before-finish，subject.key隔离，Models不会误命中。真实子进程矩阵本检查点尚未执行。

## 实际验证和失败

| 证据 | 固定版本 | 结果 |
|---|---|---|
| compile-01-environment | c29fdc2 | 环境准备exit1，缺MSVC根环境，cargo没有运行。未安装工具，未修改系统配置。 |
| compile-02 | c29fdc2 | 进程只读继承机器既有MSVC安装，cargo check --lib --locked exit0；D classifier尚未接Decoder的2 dead_code warnings保留。 |
| budget-unit-01 | 4e9b3ed | lib-test编译101，三处测试unwrap_err需要ProviderPermit:Debug；动态未执行。未加Debug/未泄露permit。 |
| budget-unit-02 | 0ec607f | cargo test --lib budget::tests --locked -- --nocapture exit0，8通过/0失败/0忽略，201过滤；D上述2 warnings保留。 |

8组实跑：coverage首配busy/旧unknown/回放/CAS；双Store真实同步竞争最后额度；settle同语义重放/冲突保持原receipt/overrun；missing/invalid/三种发送错误保留；已blocked后其他在途仍结算及NotSent退款；预算插入与network终态更新故障完整事务回滚；删账及单边terminal的status/recover/开库拒绝；SQL request scope、policy R、entry R合法数字篡改拒绝。状态矩阵使用内部真实Store命令和明确合成Provider结果，不冒称这些是HTTP端到端用量测试。所有命令UTC/版本/退出码见原json；环境准备失败记录不冒称有cargo时间。

全部target/TMP/TEMP/log位于本人活动工程树.local；机器既有MSVC只读使用，没有安装更新。原件保持，发布副本仅将本树绝对路径相对化并UTF8规范化；双hash与8份文件映射见[manifest](证据0.1/manifest.json)。没有最终五门禁、付费live、release/push、跨树自合或主仓index写入。

## 未完成及下一步

U01完整非空schema14迁移/并发/故障回滚、U04真实Engine多轮及route-check、U07三真kill/wait、旧schema fixture精确适配/回归、完整报告与最终固定组合尚未完成。支持fixture额外唯一写权已报经理待明确，不擅自跨文件。当前source/index空/no-build短冻结给经理取真实组件；经理回执后继续上述独立工作。所有失败/原件与旧任务贡献保留。
