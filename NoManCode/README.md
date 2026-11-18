# NoHumanCode 源码工作区 (`NoManCode/`)

本目录承载 **NoHumanCode** 的核心工程源码与历史兼容辅助脚本（目录名保留历史约定的 `NoManCode` 拼写，产品正式名称为 **NoHumanCode**；Git 仓库根位于上一层 `../`）。

## 全局导航

- [../PROJECT-MAP.md](../PROJECT-MAP.md)：**现行项目地图**（两条执行主链、全部 Rust 模块全景与测试矩阵）
- [../AGENTS.md](../AGENTS.md)：核心工程准则与开发规范
- [../README.md](../README.md)：快速上手与 CLI 工作台使用指南
- [rust-app/README.md](rust-app/README.md)：Rust 核心工程构建、运行与架构说明

## 目录结构

```text
NoManCode/
├── rust-app/               # 核心 Rust Workspace（peachsh 二进制 + crates/protocol 共享协议）
├── config/                 # 配置模板
├── docs/                   # 专题参考说明
├── overrides/              # 早期旧版兼容脚本备份
├── tests/                  # 早期外部脚本验证夹具
├── bin/                    # 本地产物输出目录（受 Git 忽略）
└── *.ps1 / *.cmd           # 本地启动与环境辅助脚本
```

## 快速构建与验证

核心开发均在 `rust-app/` 目录下进行：

```powershell
Set-Location -LiteralPath './NoManCode/rust-app'
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action check
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action clippy
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action test
powershell -ExecutionPolicy Bypass -File .\build.ps1 -Action build
```
