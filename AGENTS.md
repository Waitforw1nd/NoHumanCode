# NoHumanCode 开发指南 (AGENTS.md)

## 项目定位与导航

NoHumanCode 是 Windows 本机优先的 CLI AI 编程工作台（源码位于 [NoManCode/rust-app](NoManCode/rust-app)，二进制兼容标识为 `peachsh`）。当前阶段**专注后端 Host 与 CLI 交互式编码体验，暂不开发图形界面**。

- **全局架构与模块导航**：修改代码前请查阅 [PROJECT-MAP.md](PROJECT-MAP.md)，了解两条核心执行主链（进程内 CLI 工作台链与本地 HTTP/SSE Host 链）、`src/` 全部 38 个模块职责边界以及 `tests/` 测试矩阵。
- **长期产品理念**：参见 [PROJECT-BOOK.md](PROJECT-BOOK.md)。

核心工程准则：
1. **实用与简洁优先**：拒绝过度工程化、拒绝进程内模块“零信任互防”、拒绝遇到单次 I/O 或上游异常就永久锁死用户项目/数据库的自残式防守。
2. **保护用户源码原貌**：凭据保护使用 Windows DPAPI（`secrets::seal` / `open`）与精确已知密钥脱敏（`secrets::scrub`），严禁用宽泛启发式正则篡改模型输出或拦截合法业务代码写入。
3. **可审阅与可恢复**：文件修改（`edit_file` / `write_file`）前记录原文件快照并展示真实行级 Unified Diff；启动恢复将未决运行态重置为 `interrupted` 并允许用户无缝继续或回滚。
4. **直接编译与并行测试**：修改代码后直接运行 `cargo check --workspace --all-targets` 与 `cargo test`，无需重型窗口锁或双哈希账本仪式。

## 常用构建与验证命令

在 `NoManCode/rust-app` 下执行：

```powershell
# 加载 MSVC 环境并检查编译
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action check

# 运行 Clippy 静态检查
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action clippy

# 运行测试套件
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action test
```
