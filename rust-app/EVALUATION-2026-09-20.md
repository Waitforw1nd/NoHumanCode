# 🍑sh harness 全面评估报告

> 后续实机评估已发现本报告未覆盖的凭据隔离、WASM 资源与一致性缺陷。本报告中的“没有发现高危级故障”只代表当时测试范围，请以 [第二次实机评估](ASSESSMENT-LIVE-2026-09-20.md) 为准。

日期：2026-09-20  
环境：Windows x64，Rust stable 1.98.1，Node 24.19.0，当前发布实例 `127.0.0.1:3090`

## 结论

当前 Rust 主路径可以正常交付使用，没有发现阻断级或高危级故障。3090 实例健康，schema 2 生效；旧版入口仍能启动并作为回退。Rust 核心的确定性测试、Node Team/Swarm 测试和 HTTP 安全边界均通过。

## 已执行测试

| 范围 | 执行内容 | 结果 |
|---|---|---|
| Rust 单元与运行时 | `pnpm run test:rust` | 10 个库测试、8 个运行时测试通过；在线模型测试按设计忽略 |
| Rust 质量门禁 | fmt、check、clippy `-D warnings` | 通过 |
| Rust 发布 | `pnpm run build:rust` | 通过，更新 `bin/peachsh.exe` |
| Team/Swarm | 模型路由、成员归属、Swarm 并发/恢复 | 通过 |
| 技能与账户 | Skill discovery、New API 余额解析 | 通过 |
| 3090 API | health、bootstrap、settings、runs、WASM 列表、静态资源 | 通过 |
| 安全边界 | 错误 Host、错误 Origin、未授权 POST、过长幂等键、缺失 WASM 插件 | 均按预期拒绝 |
| 进程生命周期 | 同数据目录重复启动 | 正确拒绝，原实例保持可用 |
| 首次启动 | 全新目录从旧配置导入并启动 3092 | 通过，schema 2、4 条路由导入 |
| 旧版回退 | legacy 入口启动 3091 | 服务可达，当前认证策略返回 401；验证后已停止 |
| 凭据扫描 | 项目源代码、文档和配置中的常见 Key 模式 | 未发现明文匹配 |

## 未执行项目

`test-xpeach-live.mjs` 和 `test-openai-parallel.mjs` 需要当前 shell 中的测试 Key 环境变量。本轮没有设置这些变量，因此没有再次产生付费模型请求。此前已用临时 Key 完成 xpeach 模型发现、`gpt-5.6-sol` / `gpt-6-astra` 流式调用和文件工具实测；本轮 Rust 运行时回归使用本地模拟上游。

当前环境没有 `npm` 命令，项目提供的 npm 脚本通过 `pnpm run` 正常执行。这是开发环境差异，不影响 `start-peachsh.cmd` 或发布 EXE。

## 剩余风险与优先级

### P2：WASM 可观测性不足

`/api/wasm/plugins` 会过滤掉 manifest、SHA-256 或模块 ABI 不合格的插件，只返回有效列表。插件写错时用户看不到具体失败原因。建议增加诊断字段或单独的 diagnostics API，并在 UI 显示插件状态。

### P2：幂等记录没有过期清理

幂等键用于防止重复创建运行组，目前会随数据库长期保留。高频使用时表会持续增长。建议下一轮增加按时间清理、保留窗口和 SQLite 索引监控。

### P2：真实模型并行测试仍是手工门禁

真实 Key 不进入常规测试是正确的，但发布前仍需要在隔离账号上显式运行一次模型列表、双 Key 并行和工具写入测试，并保存脱敏结果。

### P3：旧版回退仍保留 Node 生态限制

3080/legacy 路径继续依赖 DeepSeek Harness Node 运行时，原版插件链的 `TEAM_NOT_MEMBER` 和命名问题不属于 Rust 主路径的回归。迁移完成前保留它是为了回滚能力，不能把它当作 Rust 版本已解决。

### P3：WASM 尚无正式插件管理页面

当前插件需要放入项目 `.peachsh/plugins/<id>`，Agent 工具和 API 已可调用。后续应增加安装、版本、SHA-256 和回滚界面，并保持默认无 host import。

## 建议的下一步

1. 为 WASM 列表补充无效插件诊断和签名校验。
2. 为幂等键增加过期清理任务和数据库体量指标。
3. 把真实 xpeach 测试放入手动触发的发布检查，不写入常规 CI。
4. 为旧版插件建立逐项迁移清单，迁移完成后再考虑关闭 3080 回退。
