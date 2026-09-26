# D-R6-01 B修订第1.2轮有界只读复核

2026-09-27，员工D GPT-6-astra。经理明确派发，仅对照固定a7b65f4450feeb57017d857eee761eee95e8533b相对0bda6c0bd372692f18665b446a565451c619bf87的本人三项，不跑测试、不写产品。D3452c67继续冻结。

三项在限定源码范围均已闭合，无新增阻断，不代表测试或最终APPROVED：

- 递归搜索策略：workspace.rs:222先核根；276起每次pop目录先verify_directory_policy，子目录push前再核，读取文件前resolve(root,relative_path,false,[])核完整路径。普通根搜索不再绕过case-sensitive子目录策略；真实反例测试仍由B补。
- Restore命令全局排他：engine.rs:1889完成原Task准入后，1896将任何active.spec.allow_commands作为Active拒绝，无须与恢复Task同根。此检查在restore_locked共同gate内、claim_restore前，两恢复路由共用，因此源码上阻止A命令与B恢复并发。已有完成receipt纯回放不受影响。
- Restore历史回放：engine.rs:1851仍先认证checkpoint，1872–1887优先处理已有receipt；Complete直接返回，Unknown/Claimed保留unknown，其他状态仍Conflict；1889才进行live根/权限准入。旧legacy或原根离线不再拦已有Complete事实，且不会重新claim或恢复。restore_checkpoint在3507–3512仍先校验其checkpoint并委派同一入口。

另看到恢复每文件动作engine.rs:2001新增validate_project_task(false,false)，补充副作用前原根身份核查；仅按源码记录，不声明消除TOCTOU。

下一第一操作：B提供正式反例测试与固定组合证据后由经理验收；D保持冻结，等待明确新复核范围。