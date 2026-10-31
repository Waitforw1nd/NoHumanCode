# NEXT-19-HOST-F01：合法Host初始化夹具修正

2026-10-31，项目经理；有效派发给原员工D，B/C冻结。依据持续目标及[attempt3失败审查](../../员工D/审查记录/第十七轮/2026-10-31NEXT-19独立attempt3失败审查与收尾.md)，不新开员工或重复任务。

## 基线与所有权

独立工作树`../nhc-d-tool-result-tests`，分支`codex/d/tool-result-tests`。基线HEAD `81b9b2bc50d71376a88fbf269ebed5b87467d74b`，NoManCode `1379ee64e8a5c96285f948664fbea7ae02d8e4ab`，Rust `ba5859e7e1ea2c7f55ec123a36150c7872467837`。D为唯一实现及本树提交执行人，经理审查与整合；main产品仍NEXT-18。本人先核三SHA、status/index/unmerged空及本任务，不重读已读历史。

唯一源码写入路径：`NoManCode/rust-app/tests/tool_result_crash.rs`。本人身份和第十七轮报告按原流程维护，不改生产源码、其他测试、权限、schema或配置协议。

## 修正要求

1. 测试新建空Store后、create_run/event之前，通过现有生产`peachsh::store::fresh_settings` API初始化合法Host配置，使用已创建的测试workspace目录。禁止直接SQL伪造config、legacy导入绕路或放宽生产空历史保护。
2. 保持原历史Task和旧tool_result、首次Host恢复为interrupted、真实kill/wait、raw/query/recovery数据库副本、第二Host重启、原事件完全相等、无工具消息/无tool_start以及原raw hash不变的全部断言。持久settings随数据库复制；不将runtime重开冒充Host重启。
3. 仅必要的引入和初始化调用；不添加镜像实现的测试、延长超时、改变默认并发或跳过用例。既有Clippy条件保持。

先固定单文件最小提交、报告完整三SHA与diff/原断言映射，完成后冻结。此阶段no-build，不运行测试、格式化或Host；经理静态审查后另发唯一完整独立attempt4窗口。原attempt3原件及所有旧现场保留。

后续验证仍为原六步（check、显式CLI build、完整lib、十四integration、all-targets Clippy、fmt），固定完整候选、原环境、首失败停。不得只跑修复用例或自主重跑。动态通过后经理核真实Host kill/重启现场及完整证据，再接原C T13/T14；不以本单文件修正提前合main。旧N18-F01及N19-F01/F02/F03/F04保持未归因。
