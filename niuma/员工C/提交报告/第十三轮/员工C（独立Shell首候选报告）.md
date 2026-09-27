# C-R13-01 独立Shell CLI与验证首候选

2026-09-27，员工C/GPT-6-astra，执行ID C-R13-01-20260927-E01。独立树../nhc-c-shell-interface，分支codex/c/shell-interface；基线6c84681f6ffc47afb521c472a181f6f0217f4fe3。候选fb086a1e8a76321588de22c2c50824545514a649，NoManCode fafd1b60bc38b2684f97060317c613b4ead3aa57，Rust22f90eced29df5fa3930ee7e8095e7dc029bbeea，提交后status/index空，源码freeze供经理merge。

唯一修改cli.rs/plugin_http.rs/plugin_cli.rs。CLI精确四ID/version映射、三合法目录组合（单files1.0、单files1.1、files1.2+shell1.0固定顺序），未知/混搭/重复/缺项拒绝；shell human说明无文件scope与OS沙箱保障、禁用不取消已准入。request ID/receipt/current匹配、unknown不自动重发及非收敛完整JSON exit1保持。

S10测试准备：两当前组HTTP独立元数据、真实service key@1.0、global key跨ID冲突、shell严格输入/token与旧files矩阵；CLI三合法目录/四映射、未知ID/version/目录重复缺项错序跨代、shell human边界、unknown手动同key与非收敛输出。旧1.0/1.1 fixture与read/list/search真Host回归保留。

S09测试准备：实际Host持久shell=false后kill+wait重启、零启动Provider/全库不漂移，恶意command精确未授权失败、零approval/tool事件/无marker/无续Provider；shell仍disabled期间独立Run真实read/list/search和经批准write四文件正控；随后显式enable独立新Run等待命令pending、无ack批准失败且无marker、显式ack后真实根目录marker和stdout/Finished。根目录命令marker位于src文件write scope外，诚实证明命令不受该scope约束。claim后spawn前崩溃/Unknown由B组件负责，本测试不冒充覆盖该窗口。

实际只运行本人三文件stable rustfmt与git diff --check，exit0；未编译/专项/五门禁。当前真实D/B API尚待经理合入，测试存在不等于通过，不stub。所有E新树输出/TMP/TEMP/target/log准备；人员报告仅主仓本人，shared index/跨树merge仅经理。

下一第一操作核经理真实组合SHA/tree/status，再专项plugin_http/plugin_cli保存UTC/exit/双hash。最终S11仅经理冻结最终完整候选后C唯一串行，失败即停，不预跑。无release/live/push/reset/clean/stash。