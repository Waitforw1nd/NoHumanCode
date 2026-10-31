# NEXT-18 C-R19 公开管理首静态候选

2026-10-31，员工 C / 6.1 Terra，执行 ID `C-R19-01-20261031-E01`。已阅读正式契约 v1.0；新独立树 `codex/c/runtime-management-interface` 初始 HEAD `9b345e0699f615846495752813b367afd21d7733`、NoManCode `906328dea19bc45b0a29c8279b931144b08144cb`、Rust `7e9f77907c5df4712d2689dd22532579d3e01e15`，初始 status/index/unmerged 空。

## 首候选

本人仅提交指定五文件，提交 `4b47a51b9a020cdfb7353f7d3efdb2703187a91b`，NoManCode tree `6d98b64b64aa6a593bf5a80b29dea7c895fed97f`，Rust tree `d0381550b515eded12b2b5ec1203979b15067a14`；提交后 status/index/unmerged 空。`rustfmt --edition 2024` 完成，暂存差异 `git diff --cached --check` 通过；这是静态候选，未构建、编译、运行测试或执行组件及五门禁。B 真实源码尚未合入，调用按正式签名准备，不造 stub；候选在当前基线上不能单独编译。持续 source/index clean/no-build，等待经理审查和真实依赖合入。

- `server.rs` 新增认证 GET list/detail、PUT enabled，固定错误和严格对象/重复键、query、GET body、PUT 4KiB 限额，保留旧入口；新增真实 Router 初步用例覆盖鉴权、origin/site、query、严格 body、回放及旧 ID 隔离。
- `cli.rs` 新增 component 三类命令和原始响应重复键校验，限定 256KiB、固定错误对应关系、封闭状态和收敛公式，校验 receipt 与请求关联、current 独立；失败/断线结果未知不自动重发。
- `tests/runtime_management_cli.rs` 编写真实 CLI 对假服务的严格响应、Busy、截断发送一次、replay/current 分离用例。`tests/runtime_management_workflow.rs` 编写真实空库 Host/CLI 禁用、kill+wait、重启、启用、回放以及 files/shell 关闭后无工具 mock Completion 的场景；另有 Provider 关闭后 route check 的 Models 回执与预算不变场景。上述均为未运行测试代码，不宣称已通过、覆盖矩阵或实际持久现场已取得。
- 说明文档更新 NEXT-17 状态并标记 NEXT-18 为待联合验收候选，解释不同状态和人工原键回放。

## 尚未验证及下一步

真实 B 类型/语义、格式及 Clippy、既有行为和上述用例均待组合后实际验证；全量 M12 恶意输入、跨四 ID 冲突、Busy 在途竞争、M13 禁用时真实准入和 M14 回执持久查询/Balance 等仍需根据正式组件实际情况补强，不能据现有静态测试声称完成。重型构建需经理固定组合并明确放行；任何失败保留原件，不越权改 B/D 文件。下一第一条操作：等待经理 review、B 本人冻结及经理串行整合，重新核完整 HEAD/双 tree 后等待精确组件窗口。主仓 index、跨树整合与联合验收仅经理。
