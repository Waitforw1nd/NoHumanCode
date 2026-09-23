# 员工 D review 1.0：插件目录与依赖解析

日期：2026-09-23。审查人：项目经理。任务：D-R1-01 / NEXT-02A 修订1；执行模型：SWE2max。关联报告：[员工 D 第1.0轮报告](<../../提交报告/第一轮/员工D（插件目录与依赖解析 第1.0轮报告）.md>)；任务契约：[插件目录与依赖解析契约](../../任务/第一轮/插件目录与依赖解析契约.md)；证据目录：[D-R1-01证据](../../提交报告/第一轮/D-R1-01证据/)。

## 结论

**APPROVED（D-R1-01 实现与冻结快照门禁范围；整合前仍需联合复核）**。

经理只读核对确认三处源码候选与 D 报告一致：

- `NoManCode/rust-app/src/plugin_catalog.rs`：1349 行，SHA-256 `1676d308ac41228642ee4c0dda550b468fe78e050b68cf72e6209cd79fbb1482`；
- `NoManCode/rust-app/tests/plugin_catalog.rs`：1506 行，SHA-256 `2367ffecec26c35036b971d7ac4a1dc89e33764c4ec4ecb44a4998f125f4d250`；
- `NoManCode/rust-app/src/lib.rs`：仅新增 `pub mod plugin_catalog;`，SHA-256 `1a23addcbd7688c90af9bd808449ed97db1637d8d9becfb1633968bb803e11d8`。

经理复核未见越权文件或 P0/P1 源码缺陷。`parse_manifest` 的严格 visitor 覆盖嵌套 object/array 重复键，`register` 在插入前完整校验且失败保持原目录，作用域按 `ScopeKey` 隔离；`resolve` 按稳定 roots/requires/provider 顺序遍历，consumer→provider 边去重后以 provider-before-consumer 拓扑输出，环诊断只从 Kahn 剩余子图沿真实边返回闭环；缺失、版本不符、歧义、UnknownRoot 和原子移除均有 typed 证据。模块只保存声明和计划，不执行 runtime/permissions/config_schema，不改 WASM ABI、Engine、Store、Cargo 或 C 的文件。

D 证据目录的 8 个门禁结果均为退出码 0，包含 17 个集成测试、6 个模块单测及工作区检查；D 报告也保留了中间失败和环境修补历史，没有把 C 中间态当作 D 的源码修改。该结论接受的是 D 冻结副本与报告证据，不等于经理本轮重新运行测试，也不等于 Git 提交、合并或发布。

## 保留项（P2，不阻挡本范围代码放行）

1. 契约建议先运行 `./build.ps1 -Action check` 建立环境，D 的脚本直接执行等价的 `cargo check --workspace --locked`；后续验证记录应写清二者关系，避免把命令差异隐藏为“完全按契约执行”。
2. 报告第 49 行、`D-R1-01证据/run-gate.ps1` 和日志含本机工作区、工具安装目录等机器绝对路径；报告第 17 行的 `/.local` 还是根绝对形式，而第 47 行使用 `./.local`。保留原始日志作为当时事实，但在归档/发布的正文和可复用脚本副本中改为相对路径，或明确其为脱敏后的证据，不把个人机器路径写成项目入口。

## 整合前置

D 只能保留本任务三处源码写入范围；经理负责暂存、提交和整合。C 当前仍有证据补强项，不能把 D 的 143 通过与 C 的 143 通过相加为产品总验收。C 修订后，经理须固定包含 C 与 D 的完整输入，串行执行联合 gate-test/fmt/clippy/wasm-check 并记录实际数量、退出码、哈希、live ignored 和环境前置，再建立 Git 基线。
