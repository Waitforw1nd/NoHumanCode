# NoHumanCode 源码目录

按用户要求，具体项目源码集中在 `./NoManCode`。目录名为 **NoManCode**，产品名称仍为 **NoHumanCode**；总 Git 仓库和项目管理文件在上一层 fufu。

```text
fufu/
  NoManCode/
    rust-app/           Rust Host、protocol、UI、测试与技术文档
    config/             配置模板
    overrides/          旧版兼容实现
    tests/              旧版及 Provider 验证脚本
    bin/                构建输出（当前为空，受 Git 忽略）
    package.json
    pnpm-lock.yaml
    *.ps1 / *.cmd       构建辅助、启动与旧版兼容入口
  niuma/                项目经理和员工档案
  references/           第三方参考资料
  .git/
  AGENTS.md / PROJECT-BOOK.md / 项目开发守则.md
```

## 开发入口

先读 [总协作规则](../AGENTS.md)、[人员身份入口](../niuma/README.md) 与 [项目书](../PROJECT-BOOK.md)，按明确任务和文件所有权继续工作。项目根和源码根不同，执行命令前核对工作目录。

以下命令从仓库根执行，进入本目录下的 Rust 工程：

```powershell
Set-Location -LiteralPath './NoManCode/rust-app'
pwsh -NoProfile -File .\build.ps1 -Action check
```

其他既有 Action 及说明见 [Rust README](rust-app/README.md)。Node package scripts 从 `./NoManCode` 运行，仍引用同级 tests 和 rust-app。

build.ps1 会把发布构建输出到 `NoManCode/bin`；`start-peachsh.cmd` 按脚本目录定位 bin、data、data-rust 和 workspace。本次仅搬迁，没有生成发布 EXE、创建运行数据或启动服务。

## 兼容与迁移记录

Cargo workspace、crate 路径、Rust 内嵌 web 路径、脚本与测试的内部相对关系整体保留。crate、EXE、数据库、ABI、header 与配置键继续使用既有兼容标识。

旧兼容入口从脚本自身目录定位源码；外部 Harness 必须配置 `DSH_RUNTIME_ROOT`，Node 使用 PATH 或 `NODE_EXE`。`DSH_HOME` 默认使用源码目录 `data/`；两个目录变量的相对值都以 `NoManCode/` 为基准。缺少的 node_modules 或 data/skills 不通过复制旧数据和凭据补齐，本次未运行真实旧实例。

迁移完整性和检查证据见 [经理源码迁移审查](../niuma/项目经理/审查记录/2026-09-23源码目录迁移审查.md)。本轮按用户要求将历史本机路径改为相对表示，旧哈希保留为当时事实；路径基准与外部历史别名见 [路径约定](../路径与可移植性.md)。
