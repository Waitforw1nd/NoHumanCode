# 参与 NoHumanCode

协作与代码贡献请从以下现行导航文档开始：

1. [AGENTS.md](AGENTS.md)：核心工程准则、安全边界与日常构建验证命令。
2. [PROJECT-MAP.md](PROJECT-MAP.md)：全局架构拓扑、两条核心执行主链、全部 Rust 源码模块职责与集成测试矩阵。
3. [PROJECT-BOOK.md](PROJECT-BOOK.md)：长期产品定位、设计哲学与阶段演进规划。

## 提交与验证规范

- 所有仓库内文档与测试夹具均使用相对路径，严禁提交本机绝对路径、真实 API Key、DPAPI 密文、构建产物或本地运行数据库（`.nhc-data` / `data-rust`）。
- 提交代码前，请在 `NoManCode/rust-app` 下确保格式、静态检查与测试套件全部通过：
  ```powershell
  powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action fmt
  powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action clippy
  powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action test
  ```
