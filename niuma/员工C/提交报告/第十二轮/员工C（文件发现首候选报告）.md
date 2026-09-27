# C-R12-01 文件发现首候选报告

2026-09-27，员工C/GPT-6-astra，执行ID C-R12-01-20260927-E01。基线11b9bee8c05a4c05396ae18206a693e15bebc26a，独立树../nhc-c-file-discovery，分支codex/c/file-discovery。候选9db6405b7161c5cc950abd5349cf72a1f0786a62，NoManCode186d46ef144e9e6ebd55d251f3e297999fba30f5，Rust a1aa21b202513d81ab1f397769e5fa4dd62d2ca1，提交后status/index空。

唯一修改src/cli.rs、tests/plugin_http.rs、tests/plugin_cli.rs。CLI精确接受1.0.0两affected/四compat与1.1.0四affected/二compat；未知版本或混搭拒绝，原PUT unknown/no retry语义保持。human从已校验DTO拼接，JSON原对象保留。server/main/schema/其余模块未改。

L07测试新增旧新版本list/status/set三命令与human范围，混搭两方向分别affected/compat、未知version与任意工具名拒绝。HTTP状态改为1.1四真实provider key@1.0，effect4/订阅0/两实例，保留原严格HTTP/CAS/故障/丢响应全部测试。L06在原read真实Host kill+wait helper增加list/search分别运行：无启动Provider、持久disabled和全表不变，恶意调用failed且精确错误/零tool_start-result/无tool消息/零续Provider，显式enable后独立新Run验证list精确文件、search精确路径行号文本。旧read正控保留，非drop Arc重开冒充crash。

实际仅stable rustfmt --edition2024本人三文件和git diff --check退出0；未编译/运行专项、未最终五门禁，测试代码存在不等于通过。依赖D/B真实1.1 API待经理合入，不造stub、不跨写。原NEXT-10结果不计本轮证据。

新树E.local/tmp/target/logs及rust-app/target已准备。源码freeze供经理merge，下一第一操作核经理组合SHA/tree/status，再定向plugin_http/plugin_cli并保存UTC/exit/原始发布双hash。最终五门禁仅经理冻结完整组合后C唯一串行。