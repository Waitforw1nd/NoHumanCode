# NEXT-19-HOST-F01 合法 Host 初始化静态修正

2026-10-31，员工 D；依据[正式任务](../../../项目经理/任务/2026-10-31NEXT-19-HOST-F01合法Host初始化夹具修正.md)和[attempt3失败审查](../../审查记录/第十七轮/2026-10-31NEXT-19独立attempt3失败审查与收尾.md)。本次为静态候选，待经理审查；不代表测试通过或 NEXT-19 验收。

## 基线、范围和固定结果

独立树 `../nhc-d-tool-result-tests`，分支 `codex/d/tool-result-tests`，执行前 HEAD `81b9b2bc50d71376a88fbf269ebed5b87467d74b`、NoManCode tree `1379ee64e8a5c96285f948664fbea7ae02d8e4ab`、Rust tree `ba5859e7e1ea2c7f55ec123a36150c7872467837`；status、index 和 unmerged 为空。D 为唯一写入与提交执行人，经理审查和整合。

提交 `2f1bc071ca4494404c8d0370604240aa29a93b55`；NoManCode tree `15f9e726327197f5105293d9997ad800a2cd4f9a`；Rust tree `e64f9c159d311b5134cd94230e9a8f71932c6205`。相对基线仅 `NoManCode/rust-app/tests/tool_result_crash.rs`，5 行新增、1 行删除（导入展开及一条调用）；文件 Git blob `cff1409d1203fd6350a166e57381a7fc392cc840`，工作文件 hash-object 一致，SHA-256 `ffb1a558e122e9e201004cde592f9e569dfe563375fd7915f22492e295e56ea1`。提交后 status、index 和 unmerged 为空。

## 改动与原断言映射

测试在创建 `data` 和 `workspace`、打开空 Store 后，通过公开的 `fresh_settings(&store, &root.join("workspace"))` 持久初始化配置，随后才构建并创建历史 run/task 和旧 `tool_result`。API 自身检查空历史并写入配置；不直接操作 SQL，不改生产代码或配置协议。工作目录随原 recovery 目录创建，持久配置通过原数据库复制流程带入第二次 Host 启动；是否动态成功仍待验证。

除引入及调用外，原测试代码未改：首次 Host 的 interrupted 断言及真实 kill/wait；raw/query/recovery 数据库复制与元数据；query 的历史结果完全相等和无 tool 消息；第二 Host 启动、interrupted 和 kill/wait；recovery 的结果完全相等、无 tool_start；raw 文件原始哈希复核均保留。测试仍调用真实 Host，而非将 Store 重开视为 Host 重启。

## 验证和后续

静态执行 `git diff --check` 和暂存 diff 空白检查，均退出 0；核对唯一变更文件、三 SHA、blob 和清洁状态。**未构建、未运行 cargo、测试、Host、Clippy 或格式化**，不声称编译或动态通过。attempt3 原件及旧现场保留；旧 N18-F01、N19-F01/F02/F03/F04 仍未归因，NEXT-19 联合验收和 C T13/T14 未完成。

下一步第一条：冻结独立树，等待经理静态审查及另行签发完整 attempt4 窗口；不自主运行或启动 C。共享主仓 index 未操作。