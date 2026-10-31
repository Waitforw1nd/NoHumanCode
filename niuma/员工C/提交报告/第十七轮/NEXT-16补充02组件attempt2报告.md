# NEXT-16 补充02组件 attempt2 报告

2026-10-31，员工 C / 6.1 Terra，C-R17-01-20261031-E01。依据[重放行](../../审查记录/第十七轮/2026-10-31补充02组件重放行.md)，本人核收后对新候选运行一次，**cargo 与外层均 exit0，1 passed / 0 failed / 0 ignored**。这仅是 NEXT-15 加 C 修订的组件结果，B/D 新内核组合和最终五门禁尚待验证、经理最终验收尚未进行。

固定本人树 `../nhc-c-first-user-workflow`、branch `codex/c/first-user-workflow`，HEAD `74f40f3e5105c8ef829d8b2d718c13431cbf0854`、NoManCode tree `fd95d00385a44d7aba1e8ae842e9f8e3c7ebbbb6`、Rust tree `183eb11234887c6f0d9946f140b7e931ef24c264`。门禁前后 status/index/unmerged 空、155 跟踪源码清单相同，SHA256 `9070EBBAE411ED8F27B1E7260EC7E5F9D391B9BC45C2F8A44BDE1CB010BAF285`；期间无产品源码更改。

在本人树 `NoManCode/rust-app` 执行 `cargo test --locked --offline --test first_user_workflow -- --nocapture`，UTC 02:33:31.6998132 开始、02:33:53.0531508 收口，session 38818 已自然结束。沿用已有 MSVC/SDK、offline、locked、debug0、incremental0、jobs2，测试原并发未改；TMP/TEMP、target、日志均在本人树。mock 与合成凭据，无付费 live、安装、release或 push。

单个故事经真实 Host/CLI 从空数据安全配置，显式 Project、预算和网络授权，读列搜、拒绝/批准写及批准 Shell，经 Git diff、checkpoint、外部变更冲突与恢复，分别验证 checkpoint 创建后/恢复后旧 Task 继续返回 conflict/nonretryable 且 Task、上下文、Provider 计数和文件不变；恢复后新 Turn、事件游标、同库重启重新 bootstrap 与再次新 Turn 的断言均执行到测试通过。Shell 所生成的独立副作用文件保持且不重复。测试日志记录两次 Host `kill+wait`，PID 42732、25800；不将这些 PID 的事后状态推断到其他运行。首次 attempt1 业务失败及证据原样保留，不被本次结果覆盖。

原始日志根 `.local/first-user-component-attempt2/`、驱动 `.local/first-user-component-attempt2.ps1` 均相对本人树；原 `test.log` SHA256 `93FE90101C28657EE2A4BDC3505F6D1D8609445EF09E50E58B1B9F5B7E616AB8`。5对原件/发布副本的字节数与 SHA256 已逐对读取核验，其中2份仅相对化路径；[清单](NEXT-16证据/组件attempt2/manifest.json) SHA256 `5AF3DC64C3220D67B80DA8A9DD4D1354D0A08D4AB7A30CDB7957D68479C7E75C`。02:34:35.5116406Z 只读扫描命令行关联本人树进程无匹配，三 SHA 和 status/index/unmerged 再核一致。本人明确持续 source/index clean/no-build 冻结，窗口结束已交经理供 D 调度；主仓 index/跨树整合由经理。

本轮无产品、协议、schema、迁移、权限或凭据更改。下一第一步等待经理新组合整合和精确放行后本人核收，再进行规定验证；不自行 APPROVED、不宣称完整阶段一或项目完成。
