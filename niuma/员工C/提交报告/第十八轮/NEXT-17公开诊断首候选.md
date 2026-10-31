# NEXT-17 C 公开诊断首候选

2026-10-31，员工 C / 6.1 Terra，C-R18-01-20261031-E01。已完整读正式v1.0，`Some([])` 为工具组、空投票后继续；Apply/Deny由Host逐项保留原前缀语义，预审旧建议不覆盖契约。初始本人树 branch `codex/c/agent-runtime-interface`、HEAD `6f22e4f8ca3356e24855962a517912cfc9ecc93b`、NoManCode `9cf2fd7a0d2e30c5cb51ce63c5ecc981a0e1d37e`、Rust `65b3da024f5e9d83ba7aaf7aed3bd86cfc27a57b`，status/index/unmerged空。采用独立树本人提交，主仓index和跨树merge仅经理。

固定首候选 `fe210cf101175b70818991ee732bff26c3ab6840`、NoManCode `60f0240416c71963c5385d50d2e14f5061ae7099`、Rust `2aa00eac8b0415aa007471eedbe668938d1dbd81`；仅 `src/server.rs`、`src/cli.rs`、`tests/agent_runtime_cli.rs`、`docs/cli-first-release-acceptance.md` 四授权文件。status/index/unmerged空，持续source/index clean/no-build。B真实运行态尚未合入，使用其已通知签名写调用，未造替身；当前候选不能独立编译，不是A11/A12通过。

## 实现与静态测试设计

- `GET /api/runtime/agents/chat` 显式纳入token认证读路径，拒绝query，沿用Host/Origin/site/安全头，读取B `builtin_agent_status`，错误固定503 `agent_runtime_unavailable`、nonretryable与安全消息。未新增Provider诊断、写控制或调试路由。
- `runtime agent status` 使用bootstrap、本机无代理/重定向/重试、封闭字段+重复键检查、固定身份版本scope、u64和last_failure白名单，并按B给出的draining、transitioning、active、准入及effect不变量检验；不打印服务器任意错误或bootstrap token。检查约束与B固定候选联测后才能确认。
- 新 `agent_runtime_cli.rs` 准备真实binary HTTP/CLI与重启、token/Host/Origin/site/query/method及no-store、格式/错误与恶意响应检测；不依赖将私有测试接口暴露到生产。`server.rs` cfg(test) 模块准备真实Router读取实际启停状态与B仅测试状态不可用注入的503；另有Windows cfg(test) A12：测试装配受限ReadOnly策略，通过真实Router公开Project Run，受控Provider观察只有read_file、阻塞期间GET检查in_flight=1，完成后in_flight=0并查Task结果。A12预置Store仅用作该组件夹具，不替代不通过Store预置的P07真实空库故事。所有代码尚未运行，不能预称验证成立。
- CLI说明更新NEXT-16已限定验收整合与诊断作用，九月原始历史统计保留；禁止把本轮描述为持久插件管理或整个阶段一完成。无schema/迁移/授权策略/凭据改动；仅新增只读诊断接口，不修改旧PluginStatus DTO。

静态检查：`rustfmt --edition 2024 --check` 对三个Rust文件通过，`git diff --check`、提交前 `git diff --cached --check` 通过。未运行编译、单测、CLI或HTTP；待B固定并由经理在双方本人冻结后整合真实依赖，实核新完整HEAD/两tree后申请唯一组件窗口，按精确放行测真实Router、二进制、CLI以及恶意响应。最终五门禁须在完整B/C/D组合固定后单独放行。先等经理审查与依赖，不自行跨树merge、不自行APPROVED。
