# Sprint 04 · 远程 MCP 授权

## S4-T1 · OAuth 登录与令牌刷新

以 Windows/MSVC 已验收的 Baseline 4 为起点，接口约定见 [ADR-009](../ARCHITECTURE-DECISIONS.md)。

| 负责组 | 交付与验收 |
| --- | --- |
| Tech-Provider | 资源和授权服务器元数据发现、授权码交换、刷新请求，以及凭据绑定。 |
| Tech-Runtime | 在现有 MCP 会话中按需刷新、保存轮换令牌、保留工具执行与审批语义。 |
| Tech-CLI | 工作区登录命令、PKCE 与本机回调、交互提示及取消。 |
| QA-Gate | 通过真实 CLI 和回环服务验证首次登录、重新启动后的刷新、拒绝和工作区隔离。 |

### 使用与覆盖

- 先在 Host 环境中登记工作区、资源端点、发行者及公共客户端 ID。使用 `coda mcp login <服务名>`，或在已打开的工作台中输入 `/mcp login <服务名>`。两种入口均显示浏览器授权链接，支持指定本机回调端口。[配置与示例](../../README.md#接入远程-mcp-工具)
- 真实 CLI 用例覆盖新数据目录先登录再运行 `ask`，以及同一聊天进程登录前后远程工具的可见性。模拟授权服务核对 PKCE、资源及回调地址。拒绝授权时，换码与工具调用次数均为零；相邻工作区仍需独立授权。
- 服务轮换刷新令牌后，重启的 CLI 会使用最新凭据。即使服务省略 `expires_in`，工具发现收到 401 时也能在本次请求中刷新一次并恢复。若刷新返回 `invalid_grant`，工作台提示重新登录。已经派发的工具调用若收到 401，则记录结果未知。[真实 CLI 回归](../../tests/support/oauth_workbench.rs)

### 验收状态

用户已确认累计补丁 SHA-256 一致，Windows/MSVC 的 `fmt`、`check`、`clippy`、`test` 退出码均为 0，S4-T1 已登记为 [Baseline 5](../BASELINE-CHANGELOG.md)。下表为服务器回归数据，使用本地模拟授权服务、资源服务和模型。

| 服务器验证 | 结果 |
| --- | --- |
| Rust 格式、Linux 和 Windows GNU 的全工作区 `check`、`clippy -D warnings` | 全部通过。 |
| Windows GNU / Wine 定向回归 | 90 个不同用例通过，含 3 个真实 CLI OAuth 场景。两个并发请求共享一次刷新；旧凭据迟到的 401 不会清除新登录。 |
| Linux 全量 `cargo test --workspace --locked --no-fail-fast` | 122 通过、39 失败、20 忽略。与已验收 S3-T3 在同一服务器环境的结果相比，失败用例名单完全相同。 |
