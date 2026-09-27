# 员工D NEXT-12第0.1轮可消费API小步

2026-09-27，员工D GPT-6-astra；D-R10-01-20260927-E01。完整基线6c84681f6ffc47afb521c472a181f6f0217f4fe3，独立树../nhc-d-shell-service、分支codex/d/shell-service。本人提交授权源码，经理独占跨树整合/主仓index。

提交：48797ca1efe609857c7ba7caa7c444492a45077d；NoManCode tree028c3b8410208e2fbf490c4ca2ba7ba3b0b3ad03；Rust treea0ccf68ac6f15d516fe2af6f3bc539c88ceabb7c。提交后status/index空，向经理报干净整合窗口。

已实现冻结D→B API：builtin_with_plugin_states双bool、with_shell_control原FilesControl签名、shell disable/unload薄控制；files1.2/shell1.0固定常量，compat仅wasm。真实shell registry与独立Host/Mutex/生命周期/计数，组间不嵌套锁。ToolLease shell分支worker拥有lease，caller只接oneshot；command_execution提供crate可见单元CommandOutcomeUnknown，统一执行deadline覆盖child和双reader，固定5秒总收尾，直接child终止/等待、reader abort/await。未保留可选taskkill helper，不承诺OS后代全灭；workspace仅command委托与旧import移除，lib仅私有模块声明。普通files路径不变。

原build.ps1 -Action check（cargo check --workspace --locked）exit0，UTC与原始日志见[D证据清单](D-R10-01证据清单.json)。target/TMP/TEMP/log均新E树.local，MSVC只读既有安装，process debug0/incremental0/jobs2，原失败/旧产物未清理。proc-macro-error2未来兼容提示保留。

组件测试/限定Clippy尚未运行；不是最终五门禁或联合验收，B持久链尚未整合。下一补S01/S02/S05/S06真实命令及生命周期反例组件，新提交不改写此固定SHA。