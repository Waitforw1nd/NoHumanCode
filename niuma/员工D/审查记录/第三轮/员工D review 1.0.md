# 员工 D review 1.0：builtin 工具生产装配组件

经理 GPT-6，2026-09-27。任务 D-R3-01 / NEXT-05 v1.0，员工实际模型 GPT-6-astra；执行标识 D-R3-01-20260927-000811。结论：**APPROVED（D 五文件组件范围，供联合候选组合；不是 NEXT-05 整体或阶段一产品验收）**。

受审候选 `3d00f121b0c66124cbb2afc2d6fdf0d1cf504c10`，基线 `5cf74319ea3b37af9c27821a6eaac944241473e4`。已读[正式报告](<../../提交报告/第三轮/员工D（builtin工具生产装配 第1.0轮报告）.md>)与[契约](../../../项目经理/任务/2026-09-27NEXT-05单Agent贯通契约.md)，检查完整五文件差异、原始定向日志及清单，候选干净冻结。经理无产品实现贡献，本轮未重跑员工测试。

## 审查结论与证据

- `resolve_bound` 读取消费者激活manifest/plan、双方Active与真实provider effect后进行typed downcast；不通过目录重解析悄悄换provider，不跨scope。错类型/版本/undeclared/stopped及独立Host测试覆盖；另一个provider非Active分支为源码核对，不冒称分支实测。
- runtime私有Host与service，payload实际决定definition/execute，替代payload的schema/marker/计数证明真实调用。两项文件工具停止后不回流旧兼容分支；list/search/command/wasm只称兼容，未宣称全面插件化。
- acquire在同一短锁内检查权限与live service，先完成可失败快照，再调用同步admit；成功只返回不可Clone、消耗执行的lease。执行前再次核task/workspace/spec，无普通Mutex跨await。Store→runtime反向锁序由B集成继续遵守。
- 停止先赢零admit；admit先赢的lease可在撤销后完成，后续调用拒绝；失败admit无lease。6项模块、24项plugin_host集成、2项runtime集成通过。这里是callback admission证据，不能替代B真实Store claim/Engine竞争。
- check/fmt/clippy均exit0，未来不兼容依赖提示保留。首次根目录cargo1.82/edition2024格式化失败已披露，改正确Rust目录后通过，未改工具链。最终workspace全test与wasm-check由联合候选执行，不需要在D重复。
- 经理SHA校验首次因脚本使用相对根的parent定位错误exit1（非产品失败），改用真实仓库根后exit0：5份原始/发布日志对及5源码文件与manifest相符；源码仍仅lib一行导出、plugin_host resolver、tool_runtime及两个测试文件。
- 归档检查发现新发布日志的文本写入生成了CRCRLF；经理仅规范本轮未归档发布副本换行/末尾空行，更新manifest的published哈希与转换说明，五份原始日志哈希保持。D身份历史正文恢复原有字节，新接手记录保留，避免无关整篇换行差异。

## 验收边界与交接

N08在本组件范围通过；N09模块部分通过，真实Engine接入与在途审批/FS事实由B证明。重建一个ToolRuntime对象不是进程恢复证据，N12由C实际Host kill/wait/restart补齐。审批、Workspace before/finish事务、unknown不重试没有被D复制到插件。

候选已按[组合记录](../../../项目经理/审查记录/2026-09-27NEXT-05候选组合记录.md)作为依赖合入B开发候选3707dd1ed6f88298a6ac213c1d08530e1598531f；main产品尚未包含本片。`execute`局部dead_code注解的中间组合原因已披露，不构成阻断，不追加无关返工。

D源码保持冻结，随后只读支持B固定差异复核可继续，结果另报；不改变D原候选与贡献归属。B/C联合N01～N14及五标准门禁全部审过后，经理才决定main整合。
