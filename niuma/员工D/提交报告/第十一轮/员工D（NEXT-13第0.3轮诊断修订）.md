# NEXT-13 第0.3轮诊断修订与四参接口窗口

员工D GPT-6-astra，2026-09-28，D-R11-01-20260927-E01。独立分支codex/d/git-worktree，D提交本人源码，经理唯一跨树整合。第0.2证据计数及受测候选限制见[勘误](第0.2轮勘误与证据限制.md)，不改原日志。

N13-D04：created内部新增require_clean；execute_claimed后验true，diagnose false。身份/head/ref/admin、安全配置及可信Git约束保持。真实staged/unstaged/untracked三个场景均PresentMatching，文件与index原样；同一结果严格创建后验仍Dirty。新增fsmonitor实际Git status脚本正控、16MiB真实配置文件超限拒绝且无副作用。

N13-D05：command的全注册根核验失败限定映射Unavailable；diagnose先核source/parent/common/admin变化→IdentityChanged，再核所有ctx注册根才能观察artifact。缺失/替换无关root在absent和真实present+registered result两路径均Unavailable，源变化优先IdentityChanged；无root剔除。经理指出首d60仅absent测试不足后，在8f1e8fd补全present前置核验与反例。

证据：attempt8修改树15/0；固定d60bf07357a4c8faa8fb8a83a5f550ef462e136e attempt9为16/0；固定8f1e8fd在attempt10为17/0（完整SHA/tree以元数据为准），154 filtered out，无忽略。attempt8起元数据含full HEAD、两tree、status及源文件SHA256。d60两文件fmt0。B cfg(test)未使用helper警告保留。最终五门禁未执行，环境及项目git.exe注入专项仍待补。

按经理B02裁决增加diagnose第四参registered_result_git_dir；created实际观察的git_dir与保存admin完整身份比较，不等IdentityChanged，不限定admin basename。此接口修订3964068c61a6746f5993e3af6d836dc3abe27e6b / NoManCode95ebf92247212773913dda5b97d9ff8314487b3e / Rust4db0cd9b0bafa55206a0cc3cb9f5f606978676a9，尚未编译，等待经理合B真实Engine第四参适配；没有越权改Engine或stub。

提交后status/index空、无构建，源码冻结并向经理开放整合窗口。新增8对证据见[evidence-0.3清单](evidence-0.3-manifest.json)。下一第一操作：核收经理B→D完整组合，真实编译及限定组件/交叉admin验证；再补剩余注入边界。原始日志保留。