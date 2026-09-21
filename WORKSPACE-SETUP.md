# 🍑sh harness 干净开发目录

目录：`D:\code\fufu`  
创建日期：2026-09-22

这是从 `D:\peachsh-harness` 复制出的新开发目录。原目录仍保留，作为运行中的旧实例、数据和回退来源。

## 已复制

- Rust workspace：`rust-app`；
- 配置、覆盖和测试目录；
- 项目书、方向草案、交接文件和架构基线；
- 参考项目和 DeepSeek Harness 资料；
- 构建脚本、协议、源代码和测试夹具。

## 有意排除

- `bin`：已编译发布文件；
- `data`：旧 Node 数据；
- `data-rust`：正在使用的新 SQLite、配置和加密数据；
- `workspace`：旧项目工作区；
- `node_modules`；
- `rust-app\target`：构建产物；
- `references\dsh-v0.1.6-alpha.2.full.zip`：重复的压缩参考包。

## 开工规则

1. 先在此目录建立 Git 仓库或明确的变更隔离方式，再开始多模型并行修改。
2. 不复制旧数据、旧凭据或任何真实 API Key 到此目录。
3. 新实例应使用独立的数据目录和工作区，不能与旧 Rust 实例共用 SQLite 或 WAL。
4. 先读取 `PROJECT-BOOK.md`、`HANDOFF-TO-NEXT-MODEL.md` 和 `rust-app\MASTER-ARCHITECTURE-BASELINE-2026-09-22.md`。
5. 当前路线是后端优先的 CLI 开发者版，然后是 Desktop 友好版。
