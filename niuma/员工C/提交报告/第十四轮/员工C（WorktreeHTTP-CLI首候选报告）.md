# 员工C：NEXT-13 HTTP/CLI 首候选报告

2026-09-27，C/GPT-6-astra，正式任务C-R14-01-20260927-E01。完整读取v1.0契约及本人任务，独立树../nhc-c-worktree-interface，codex/c/worktree-interface，基线32b9bfaca59a3f87444d20ed705d01d4678e7f46。原PRE与R13不恢复。

首候选提交 **674f4e33ba504866833fcd79946721a1c5ceef34**；NoManCode **e1968ba4cc9ff9bdfcd9b6f0af9d575ffe339505**，Rust **b16ae99cd8a0a257178d5b151df5a91dff2a3602**。四个唯一所有权文件：src/cli.rs、src/server.rs、tests/worktree_cli.rs、tests/worktree_http.rs；无lib/Store/Git组件修改。提交后status/index空，无持续构建，提供经理普通merge冻结窗口。

## 实现与测试代码准备

- HTTP create/status/lookup/inspect四入口，strict重复/未知字段防线，全部走现有/api/projects认证前缀。typed安全错误映射，不传播底层stderr。创建200/202/409按持久state，读正常200不假定succeeded。公共DTO使用B2060865固定定义，未在C树伪造Engine/API。
- CLI project worktree create/status/inspect；status --id/--key互斥必选。所有Git/DB/建目录通过Host，不本地旁路。生成branch/leaf、UUID/OID/revision/时间/state/project/cause严格验证；Option字段也要求wire明确存在。expected_head输入大小写规范比较，parent回执逐字原输入；target语法比较支持正常Windows slash/drive大小写/标准扩展前缀，不FS canonicalize。
- create发送后丢响应/畸形/截断/重复或不一致DTO/打印失败→operation_result_unknown，原key手动查询，无自动重发；完整claimed/unknown/rejected回执stdout且非零退出。lookup/inspect只读POST错误不冒称产生新副作用。human明确共享Git数据和独立授权边界。
- W09 CLI实际子进程测试代码：各状态、认证wire、输入拒绝、回执矩阵/重复字段、HTTP码、安全enum、inspect一致性、broken stdout、路径语法兼容、无本地文件变化。
- W01/W02/W09/W10实际HTTP测试代码：真实Git建树/branch/common/HEAD/index，创建Provider计数不增，新Project默认无权、源grant不能跨Project、新grant后审批写入新根、原Session新Turn/绑定不换根，多次inspect/status/lookup/replay数据库不漂移，DTO/auth/跨source反控。
- W08 server.rs cfg(test)模块准备真实含Engine/Router的lib-test子进程，调用B批准的per Engine三停点：claim后spawn前、Git后登记前、登记后响应前；父请求在途时真正kill+wait，重启查询/回放/inspect状态、未知预约不释放、Project不重复、source/index/branch不变。不是生产binary环境注入；B owner hook尚待真实依赖合入。hook等待20秒、父通知10秒、child wait5秒，无生产测试后门。

## 验证与限制

仅执行 `rustup run stable rustfmt --edition 2024` 限定四文件（rustfmt1.9.0 stable）与 `git diff --check`，实际exit0。**尚未编译、专项或最终五门禁；上述为测试代码覆盖设计，不是动态通过**。B/D实现与hook由经理合入后才能编译，禁止假桩掩盖；最终五门禁仍未放行。现存前置建议保持冻结，源码提交只本人树。

下一第一操作：经理合真实D/B后实核新完整SHA/tree/clean，再运行固定专项，失败保留原件/UTC/exit/双hash，仅修本人文件；跨owner问题交经理。当前源码冻结供经理整合。
