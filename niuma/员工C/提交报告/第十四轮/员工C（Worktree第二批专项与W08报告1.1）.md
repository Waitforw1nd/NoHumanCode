# 员工C：NEXT-13 第二批专项与W08报告 1.1

2026-09-28，C/GPT-6-astra，C-R14-01-20260927-E01。经理将D e839合入后，实核327e47564e5e84340f2d2978ce5e251d3c806821 / NoManCode4ae1ac8be7e63620a4e092e0e0cbd97026dabf92 / Rust419969c978efd0034048a0488d2f942e1bf44427，status/index空，再恢复组件专项。未运行最终五门禁；B交叉review后修订尚未合入。

## 实际结果与失败保留

| 标签 | 固定版本/命令 | 实际结果 |
|---|---|---|
| 327e475-worktree-attempt3 | 327e475；cargo test --locked --test worktree_cli --test worktree_http | exit101；CLI8/0/0，HTTP1/1/0 |
| worktree-http-attempt4 | 7073245；cargo test --locked --test worktree_http | exit101；HTTP1/1/0 |
| 7073245-w08-attempt1 | 7073245；cargo test --locked --lib server::worktree_http_crash_tests::w08_real_router_process_kill_wait_at_three_durable_boundaries -- --exact --nocapture | exit0；1/0/0，167filtered，无空helper |

每个日志meta完整保存受测全SHA/两tree、命令、UTC、环境、exit及前后clean。所有原件保留，不合并多轮计数冒充同版通过。

attempt3：真实clean Git创建已经成功，W09认证/严格DTO/跨source完整通过，关闭C单一错误token夹具问题；W02的无grant Run实际同步409 required/not_sent/calls[]。C前次静态判断只看Engine launch而漏了Store commit_turn_bundle的bind_task_tx预检，74f9关于200后failed的预期错误。仅将本人fixture改为严格409及全DB不增长/Provider不增加，提交7073245；保留原静态记录和本次失败，不将其掩盖为生产缺陷。

attempt4：上述修正后，W01创建、源HEAD/index/文件不变、linked关系、新Project默认无权限、源grant不能跨Project、新grant后审批写新根、原Session新Turn/旧绑定保留均已到达；最后写后inspect预期PresentMatching实际Partial导致失败。经理裁决N13-D04：普通dirty内容不应被当作Partial；D修诊断与创建后验clean策略分离。C保持原PresentMatching强断言，不删除已写文件、不改成Partial、不改D模块。此场景整体仍失败，局部已到达不代表W01/W02全通过。

## W08实际范围

7073245唯一父测试遍历三个真实含Engine/Router的lib-test子进程，分别在B owner cfg(test) ClaimCommitted、CreatedBeforeRegister、RegisteredBeforeReply回调通知后杀死并wait进程。不是生产binary环境后门，不用drop Arc冒充崩溃。每例另起新Router进程、执行Store recover并使用真实HTTP读取/原key回放/lookup/inspect。

claim与created两例重启unknown且无Project；registered例持久succeeded且恰一新Project。反复查询/诊断/回放不改DB、source index/HEAD和branch不重复；unknown继续阻止普通Project注册。重启worker hook未再次触发，runs/network_calls计数0；本测试没有Provider socket观测，不能把该计数证据扩为OS网络隔离证明。

真实CLI二进制在unknown预约时project create与run start均返回worktree_busy/retryable=false，无stdout回执与DB变化，支撑N13-C01旧入口确定拒绝分类。该lib测试要求先构建伴随integration targets，使同target/debug的CLI二进制存在；本次已按顺序实际执行。测试1项包含三场景，不计成3个独立test，子进程不额外加测试数。

## 当前冻结与证据

通过W08后，只补创建后clean阶段PresentMatching正控及父进程可读阶段日志，提交 **c54c1e6b226f3fbf143d0510798a98fb46c24ee2** / NoManCode **3685d5100244c21be3d56c905fdbec2c8a0e149e** / Rust **3efbe644db94d85cc30750f4bd25d29e27a68766**；限定rustfmt/diff0，提交后clean/index空、无构建。本提交尚未动态运行；先前707 W08结果不冒称c54结果。

[第二批六对原始/发布hash](C-R14证据/第二批专项/manifest.json)已逐对核验，原始.local/logs不改，发布仅路径相对化/MSVC占位。加首批本C轮当前12对。身份已更新，明确freeze窗口交经理供D04及B真实修订整合。下一第一操作实核新组合，再HTTP完整正反控/W08/限定Clippy，最终五门禁等待单独放行。
