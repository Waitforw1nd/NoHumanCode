# NEXT-14 员工B正式任务

2026-09-28，经理GPT-6。执行ID B-R13-01-20260928-E01，唯一模型GPT-6-astra；当前代理/root/b_budget_review。旧NEXT13任务已结项；PRE只读评估已APPROVED，新任务以真实接收为开工事实。

必须完整读取主仓经理任务/2026-09-28NEXT-14预算准入与用量诊断契约.md（含补充01 v1.1），再读主仓2026-09-28NEXT-14执行登记.md，核其中完整开发基线、源码两tree和自身status/index。当前任务单不以写出代替接收。基线SHA由执行登记在管理基线提交后填入，填写之前不得实现。

Git模式：独立工作树../nhc-b-budget-ledger，分支codex/b/budget-ledger。本人是该树范围内源码唯一修改/暂存/提交人；经理唯一主仓index、档案提交和跨树整合人。人员档案只能主仓niuma/员工B/本人身份与提交报告/第十三轮，不得改员工工作树中的niuma副本，不写经理文档。当前范围：预算DTO/schema15/账本、network原子claim与收尾、Engine、恢复与原所有权fixture；精确文件见契约§7。逐文件唯一所有权与U01–U11按契约；超范围依赖先报经理，不自行代改。

先回实际完整HEAD、NoManCode/Rust tree、clean/index、执行ID、完整契约已读及第一步；有疑点继续独立部分，不能假称已实接。实现任务含本人必要组件测试，不含立即执行最终五门禁。公共DTO与D classifier先小步提交供经理审；真实依赖由经理串行普通merge，不复制源码或假桩。提交前审本人diff及限定文件列表，不git add全树。

每次shell必须显式E盘本人工作目录，所有target/TMP/TEMP/log仅本人E树.local；不改变系统HOME/CODEX_HOME，MSVC只读不安装更新。首次构建环境按主仓环境指南，Rust cwd实际edition2024/toolchain，不用旧单文件fmt代门禁。禁止release、付费live、push、reset/clean/stash和顺手清理，保留原失败/未执行/旧产物。所有构建结束保留完整原日志、命令UTC/exit/候选SHA/two tree和hash；发布副本只相对化路径并另记原/发布hash。

跨树merge前本人必须明确当前source/index clean且no-build冻结，经理实际核后普通merge；收到经理完整HEAD/tree后本人再实核回执才继续。不得自行合其他树。

交付：主仓本人提交报告/第十三轮/NEXT-14提交报告0.1.md起按轮次新增；列版本、U项实证/未执行、所有失败、文件清单、残余限制、原证据和发布hash映射。review归本人审查记录/第十三轮，由经理写。每轮实质工作更新本人身份，旧贡献保持，不自行APPROVED或宣称阶段一完成。