# 员工C：审批 schema 7 回归适配 第1.0轮报告

日期：2026-09-25；提交人：员工 C；任务：`C-R2-AP12-SUPPORT`。本轮使用用户指定的 GPT-5.6-sol，候选已提交，等待项目经理审查和串行整合；不声明 B 候选已经验收，也未启动 C-R3 审批 HTTP 开发。

## 1. 基线与范围

| 项目 | 实际记录 |
| --- | --- |
| 继承候选 | `50de5456b8c0d412aff4e90821bdde221f066a2d`（B 待审依赖） |
| 工作树 / 分支 | `../nhc-c-approval-regression/` / `codex/c/approval-regression` |
| 本轮完整提交 | `891263a4e9628affbf071a7d2ceb8ee71ef137c8` |
| 源码写入 | 仅 `NoManCode/rust-app/tests/http_contract.rs` |
| 档案写入 | 共享仓库 `niuma/员工C/`；未写任务工作树的 niuma 副本 |
| Git 状态 | 提交后任务工作树干净；未 push、合并或发布 |

本轮先读取根协作规则、本人身份、经理并行收口任务、B 第1.0报告以及审批契约修订1/2。B 报告记录 schema 6→7 和 AP12 中旧 HTTP 测试固定版本失败；这些是依赖候选事实，不等于经理验收结论。

## 2. 修改与断言保持

`h1_read_shapes_and_security_headers_stay` 的健康响应 `schema_version` 预期由 `6` 精确更新为 `7`，与候选 `store::SCHEMA_VERSION` 和真实响应一致。没有修改 `server.rs`、审批生产逻辑或新增 HTTP 路由。

同一测试及整份 `http_contract` 中原有断言全部保留，包括：安全响应头、读取响应形状、非零 schema、损坏数据库时结构化 500、错误响应不泄露 `schema_version`、Host/Origin/Sec-Fetch-Site/token/content-type 拒绝、无副作用计数、SSE、游标、隔离和旧 Run 幂等。未删除、ignore 或放宽迁移/安全断言。

提交前 `git diff --cached --check` 退出 `0`；暂存差异为一个文件、1 insertion / 1 deletion。文件 blob 为 `f7b6dbe77d88b9c24945e9ef5b1ccfbda98a8265`。

## 3. 实际验证

运行环境为 PowerShell 7，进程设置 `VSINSTALLDIR` 指向本机 Visual Studio 安装，并使用独立 `CARGO_TARGET_DIR`。未修改 `build.ps1`。

可点击证据：[验证记录](C-R2-AP12-SUPPORT证据/验证记录.md)。本轮终端输出没有重定向为原始日志，命令绝对起止时间也未采集；附件明确区分终端实际返回摘要、可核对 Git 时间锚点与未留存项，不补造 `.log` 或时间戳。

| 命令 / 动作 | 退出码与结果 |
| --- | --- |
| 初次直接 `cargo test -p peachsh --locked --test http_contract` | `1`；全新 target 未经脚本初始化，`link.exe` 不在 PATH，未进入测试 |
| 点入现有 `build.ps1 -Action check` 后运行同一定向测试 | `0`；标准 workspace check 通过，`http_contract` 11通过、0失败、0忽略 |
| `build.ps1 -Action fmt` | `0`；格式检查通过 |
| `build.ps1 -Action clippy` | `0`；workspace all-targets、`-D warnings` 通过；仅有既有依赖 future-incompat 提示 |
| `git diff --check` / `git diff --cached --check` | `0` / `0` |

未运行全仓 `test`：经理任务将固定组合版本的全仓联合门禁安排为串行执行，且 B 的 `store.rs` / `runtime.rs` 三项适配在独立分支进行。本轮也未运行 wasm-check，因为唯一变化是本机 HTTP 测试版本预期，不属于 WASM 输入。未运行付费 live、发布 build，未杀其他员工进程。

## 4. 交付状态

本候选解决 C 所属的 AP12 旧 HTTP 固定 schema 版本失败，定向回归已通过。B 仍负责其独立 `store.rs` / `runtime.rs` 适配；A 的审查和经理验收结论尚未回填。本提交需要经理审查后再串行整合，不能据此宣布 NEXT-02C、AP12 或 B 候选整体通过。
