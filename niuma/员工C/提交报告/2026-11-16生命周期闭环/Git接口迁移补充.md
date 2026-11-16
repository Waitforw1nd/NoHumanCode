# Git 私有接口回归迁移

2026-11-16，承接 C 首候选 `7f89a5c5210af8a864823145cb1f6191cad6ad94`。B 已冻结 `execution_authority::test_scope(Future)`：真实 Store/Engine/整项票，非成功桩；B 在 lib 注册 `git_diff_contract_tests`，C 只改指定独立 tests 路径。

原 `tests/git_diff.rs` 的15条测试迁入 `tests/support/lifecycle/git_diff_internal.rs`，13条 async 测试加真实能力 scope；保留原测试名称、patch/路径/并发变化/凭据脱敏/传输限制/时间界限断言。原因是原 target 直接 `#[path]` 编译生产 Git 模块；接入 crate-private 能力后不再能绕开 owner 独立执行。私有 builder、transport 与 read_with_barrier 继续直接调用产品实码。没有新增公开任意 exe/env 入口。

原外部 `git_diff` target 改为公开兼容回归：同一个真实仓库的裸 read_git_diff 必须拒绝，Engine::git_diff 必须获得真实 patch。它与 lifecycle target 的原生 Git 成功/普通失败正控共同覆盖公开调用迁移。

防止假通过的具体补充：

- bounded_runner 使用从 PATH 解析的绝对 pwsh.exe、受控 cwd、env_clear 与必要 OS 环境；仍保持原3秒/100毫秒运行预算与6秒/3秒耗时断言。超限必须实际写出 overflow-started。两次调用都给同一 ProcessSpec 附测试观察钩子，要求实际 process handle/creation identity、原对象 signal、Job 空和双 EOF/join；启动前 Unavailable 不会通过。
- 依赖 B 仅测试构建的 `test_process_once(root, spec)` RAII 一次注入，及 `GitOutput.code` 的 crate 可见性。测试没有访问生产任意执行入口。
- 原环境污染子测试名称改为 `git_diff_contract_tests::malicious_environment_child`，父方另断言 stdout 含 `running 1 test` 与 `1 passed; 0 failed`，排除匹配0条假绿。
- 夹具 Git setup、merge、环境子测试从 std output 改为实际 A Supervisor，显式环境、隐藏创建、15秒运行/5秒回收界；原 Job/process/readers 成功后才返回 Output。Unknown 停驻同一监督进程，保留原对象。两个 transport 正负控也走同一生产 run_command collector。
- 夹具目录去除 Windows verbatim 前缀，保持 E 盘实际路径，避免 Git 的 sh alias 将该前缀当网络路径；不改产品路径检查。

原生 Git 后代、关闭、unknown 的整项票补证由 B 在其内嵌测试通过唯一根路径一次 spec 注入承担，C 已明确给出原对象、在途票数与关闭后拒新断言。Shell 调用 Git alias 始终仅计为 Shell 入口进程树，不冒称原生 Git 证明。

本次仅 rustfmt、差异审查及测试名映射；未 Cargo 或真实进程运行。经理可执行 `cargo test --offline --locked --lib git_diff_contract_tests:: -- --nocapture --test-threads=1`，再执行 `--test git_diff` 与 `--test execution_authority_lifecycle` 的独立窗口。
