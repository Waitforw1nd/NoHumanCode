# worktree fixture 同域操作适配

D 自有树普通合并 B `090a0e81`（已含 worktree fence）及 A `825155e0` 能力基线后，仅改 `src/worktree_management.rs` 的测试。针对首 full 中直接 claim/finish 因缺 operation 返回 Unavailable：在已有真实 Engine controller 上以 A 的 `test_with_active_store_sync` 创建同域 operation，跨 Store 竞争的两个线程各自取得原域操作票，注册/claim 竞争也在各自 scope 内；保留单赢家、Busy/Conflict、幂等回放、真实 Git 计划、资源保留、恢复和原业务断言。未修改生产接口或削弱断言。

覆盖首轮失败的 terminal/recovery、保留状态、持久化损坏、diagnose、两个双 Store 竞争、注册竞争和 sibling reservation；具体通过与否待经理新候选验证。A/B 合并是 D 分支依赖，供经理按固定提交审查整合。仅 rustfmt 与 git diff --check；未运行 Cargo/真实 Git 测试。
