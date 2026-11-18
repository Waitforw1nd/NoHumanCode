# NoManCode 源码目录开发指引 (AGENTS.md)

本目录属于上一层仓库根目录的 **NoHumanCode** 项目：

- **总 Git 根目录**：`..`（切勿在本子目录初始化第二个 Git 仓库）
- **核心 Rust 工作目录**：[rust-app/](rust-app)（二进制与包兼容标识为 `peachsh`）

开始修改代码前，请统一查阅根目录文档：
1. [../AGENTS.md](../AGENTS.md)：项目核心工程准则与安全要求
2. [../PROJECT-MAP.md](../PROJECT-MAP.md)：源码模块全景、两条核心调用链与集成测试矩阵
3. [../PROJECT-BOOK.md](../PROJECT-BOOK.md)：长期产品理念与架构边界

日常编译与检查请在 `rust-app/` 下直接运行 `build.ps1`（`-Action check` / `fmt` / `clippy` / `test` / `build`）或标准 `cargo` 命令。
