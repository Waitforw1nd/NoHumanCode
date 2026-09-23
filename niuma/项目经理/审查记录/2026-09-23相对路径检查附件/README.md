# 相对路径与外部工具发现检查附件

日期：2026-09-23。检查对象为 [NoManCode](../../../../NoManCode/README.md) 中本轮修改的七个启动、配置和验证脚本。

附件保留此前实际通过的两套临时验证脚本及其断言，只将仓库入口和夹具目录改为运行时解析。`RepoRoot` 默认从本附件目录向上四级定位仓库，也可显式传入；夹具在系统临时目录中新建，不复制运行数据或凭据。请用独立 PowerShell 进程执行，不要点源脚本。

| 附件 | 已通过结果 | 范围 |
| --- | --- | --- |
| [verify-script-paths.ps1](verify-script-paths.ps1) | PowerShell 7 下 15 项 PASS，其中 14 项 mock 与 1 项 Node 语法 | 相对/绝对运行时与数据目录、不同调用目录、空格/括号路径、Node PATH/覆盖、缺依赖错误、newapi 退出码、缺凭据文件护栏、discovery 模块根 |
| [verify-build-discovery.ps1](verify-build-discovery.ps1) | PowerShell 7 下 6 项 PASS，Windows PowerShell 5.1 下同组 6 项 PASS | 环境变量优先、VSINSTALLDIR、vswhere、注册表、版本排序、不完整安装跳过、Hostx86 回退、缺 MSVC/SDK 在 Cargo 前失败 |

合计 27 项 PASS。另对产品的 `apply-overrides.ps1`、`set-peachsh-key.ps1`、`rust-app/build.ps1` 执行 PowerShell AST 语法检查，全部通过。构建脚本的 Action、Cargo 参数、退出码与产物复制逻辑已核对保持。

完整 27 项结果来自归档前的临时版本；机器实际临时路径不写入本附件。归档后仅运行语法与入口定位检查，没有重新运行完整 mock。以下是同组断言的可复验命令，从仓库根执行：

```powershell
pwsh -NoProfile -File './niuma/项目经理/审查记录/2026-09-23相对路径检查附件/verify-script-paths.ps1'
pwsh -NoProfile -File './niuma/项目经理/审查记录/2026-09-23相对路径检查附件/verify-build-discovery.ps1'
powershell -NoProfile -ExecutionPolicy Bypass -File './niuma/项目经理/审查记录/2026-09-23相对路径检查附件/verify-build-discovery.ps1'
```

归档后实际执行的轻量入口检查命令：

```powershell
pwsh -NoProfile -File './niuma/项目经理/审查记录/2026-09-23相对路径检查附件/verify-script-paths.ps1' -CheckEntry
pwsh -NoProfile -File './niuma/项目经理/审查记录/2026-09-23相对路径检查附件/verify-build-discovery.ps1' -CheckEntry
powershell -NoProfile -ExecutionPolicy Bypass -File './niuma/项目经理/审查记录/2026-09-23相对路径检查附件/verify-build-discovery.ps1' -CheckEntry
```

归档脚本也执行了 PowerShell AST 语法检查。`-CheckEntry` 在创建夹具之前返回，只确认默认仓库根与所需源码入口存在。Windows PowerShell 5.1 的 `-ExecutionPolicy Bypass` 仅作用于该验证进程，不修改系统策略。

未执行产品测试、真实 Cargo/编译器、真实 DeepSeek Harness、部署、发布或付费调用；未读取任何凭据。构建检查替换了工具发现和 Cargo；路径检查只运行临时 mock 程序。旧 Node 依赖和 `data/skills` 缺失仍是现状，附件不将它们伪装成已迁入仓库。
