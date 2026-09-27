# NEXT-13 第0.2轮组件修订与动态证据

更新人：员工D GPT-6-astra；2026-09-28；执行ID D-R11-01-20260927-E01。

基线为经理整合的 7956e4916bdd630b20b2a97d367c3480fda67979；Git模式独立工作树 ../nhc-d-git-worktree、分支 codex/d/git-worktree。源码提交执行人D，跨树整合人与最终验收人为经理。

候选 e8391d2024916f1c464fafc6ababc6a286152fa6；NoManCode tree bcec3f093a863cf86ae66aa7f99508f8bfe33c4e；Rust tree 4effbc5ca7907a9fd3b3e44d43bfb76aa424e136。本次只修改 git_worktree.rs。提交后status/index为空，无构建进程，已向经理开放整合窗口；源码冻结。

N13-D03根因是Git config --file不接受Rust canonical Windows verbatim路径；新增Git参数路径转换，身份持久化继续保持原始规范化身份。N13-D01已注册结果精确豁免与D02缺失目标的残留admin检查在真实工作树fixture验证。额外检查index assume-unchanged/skip-worktree隐藏状态，限制命令输出及收尾。

验证：check-attempt2/4/5均0；tests-attempt4六项通过；tests-attempt7十三项通过、0失败、0忽略，154 filtered out。覆盖真实创建及注册诊断、同父目录目标、删除目标保留admin、detached/case alias、dirty/expected head/已有目标、特殊仓库/父目录替换、受限配置/tree modes、hooks/filter负控、真实子进程nonzero/timeout/overflow/inherited pipe等。过滤器smudge/process和hook有真实Git脚本正控；不能将尚未补充的fsmonitor独立正控、环境/可执行路径注入专项、真实大配置上限等宣布已完成。W06 caller-owned worker归B契约，D进程组件测试不替代B的Engine责任。

最后两文件rustfmt --check及git diff --check为0。未执行最终全量五门禁；仅C负责。B cfg(test) set_worktree_probe_for_test未使用警告仍保留，未跨权更改或压制。

失败历史全部保留：check-attempt1为B E0308；check-attempt3为D局部git_path名称遮蔽；tests-attempt1为2通过4失败且W01失败时负控不算安全证据；w01-debug-attempt2确认config路径失败；tests-attempt5为11通过2失败，路径别名字符串与过滤器shell路径两处fixture错误；tests-attempt6为D fixture修复字符转义编译错误。上述均已由后续成功候选修复，但历史不删。check-attempt3补记元数据仅有记录时刻，不伪造运行起止。

[证据清单](evidence-0.2-manifest.json)列出22份原始/发布文件SHA256与路径相对化变化；原始保留在独立树.local/logs，发布副本在evidence-0.2。该轮为解阻集成检查点，非完整专项或最终验收。

下一第一操作：等待经理整合窗口结束/固定review，然后按明确范围补余下W04/W05证据与问题修订。诊断对用户后来修改的工作树当前仍沿用clean检查语义，待经理确认诊断应否仅核身份/head/ref。无自行merge、共享index、最终门禁或清理旧树。