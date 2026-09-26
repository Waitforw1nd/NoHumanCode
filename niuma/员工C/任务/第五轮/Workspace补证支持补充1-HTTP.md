# C-R5-CRASH-SUPPORT 补充1：独立 W13 HTTP 矩阵

2026-09-26，经理 GPT-6 在真实崩溃补证与只读盘点后派发，员工 C 实际模型 GPT-6-sol。仍在当前 C 支持任务/工作树/分支续接，避免重复执行者。原四个崩溃场景验收保持。

- 基础实现固定 `74229736ffca4f712a7564b3084110c51c58ea44`（含 B 修复、CLI 和 C 原崩溃测试，未验收 Workspace）。经理在员工确认干净后同步至 `../nhc-c-workspace-crash/` 的 `codex/c/workspace-crash-tests`，作为支持新基底。
- 新增唯一写权 `NoManCode/rust-app/tests/workspace_http.rs`，只做 W13 真 HTTP 集成测试。原 workspace_crash.rs 已冻结，无需重写。B 不写该新文件，继续其 engine/store/repository/server/workspace_changes 原范围；C 不改生产代码、其他旧测试/Cargo/构建脚本。
- 真实临时 Host、mock Provider/批准 write_file、临时 DB/文件目录；GET changes 和 POST restore 验证成功 DTO、完整 receipt、partial/unknown 409 receipt、稳定 restore_id、普通内部错误固定500/安全code-message-retryable、非法ID400/未知404/空任务 complete0 区分。验证 Host/Origin/token 拒绝及安全头，拒绝前后 DB/文件状态零变化；敏感 before/SQL/路径/密文不出 HTTP或相关事件/SSE。
- 多路径恢复途中故障用夹具先证明第一路径已完成而第二路径失败，返回 partial 的逐路径事实不得伪造。可在临时 DB 触发器/Windows文件锁配合建立确定性窗口，不加生产后门。单纯修改数据库造状态不能替代真实 HTTP 操作。
- 核对现有公共 HTTP 中间件语义，不能要求 GET 不存在的 token 语义，不能把其他路由的旧测试当新接口证据。遇实现缺陷及时交 B/经理修复，不弱化断言。
- 独立 target；新增文件定向测试/fmt/适用Clippy，固定完整提交 SHA 和命令/起止/退出码日志。最终组合全量五门禁由 B 串行完成，C 不重复跑发布 build/付费 live。报告共享 `niuma/员工C/提交报告/第五轮/` 新增，不覆盖已交历史；共享 index 仍仅经理操作。

B-F10 与前 FS 的独立只读复核可在收到新固定 SHA 后补做；经理复核 C 自己的测试。提交不等于完整 Workspace 验收或主线合并。
