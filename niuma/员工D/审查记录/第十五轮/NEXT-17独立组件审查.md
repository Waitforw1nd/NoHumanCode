# D-R15 独立组件审查

2026-10-31，项目经理。结论：APPROVED（A01–A10 独立测试及证据范围）；完整 NEXT-17 与最终五门禁仍待 C 固定组合结果。

受测 `2964062c615ba04ba097c45d48887161251937a3`，NoManCode `442e75c259d03b3e01155ff0184fa3a0b383da18`，Rust `1d660ccc91085a931e4ac142a0d870d9a7d81002`。经理已读[独立组件报告](../../提交报告/第十五轮/2026-10-31NEXT-17独立组件报告.md)、原日志、metadata、静态候选及两次测试修正，未重跑产品测试。

## 结果与证据

- check exit0；27 passed / 0 failed / 0 ignored，不累计子进程或失败轮次。
- all-targets Clippy exit101，仅 B engine.rs 两处 cfg(test) 回调类型；fmt 未执行。原编译失败及 26/1 保留。D 没有跨 owner 改产品。
- 377 对原/发布文件的 SHA256、长度全部匹配；247 份文本转换逐字节还原检查仅已知路径表示，其他原/发布字节一致。发布 runner 的 SDK 正斜线路径已补齐，原件不变。manifest hash `CF71EE2FCBF826B5715BC3C20A641C8888A4F1FB0F2A6BC371E44E38F017FCA2`。
- attempt3 三步的六份前后源码清单各 127 文件，完整集合与 Git 跟踪文件相同，当前字节全部一致，前后 clean。记录见[组件证据核验](../../../项目经理/审查记录/2026-10-31NEXT-17-D组件证据核验.json)。

## 重点断言

A09 原错误是故意创建孤儿审批后仍 unwrap 安全读取，安全接口本就应报 CorruptState。修正同时保留新组冲突错误、原消息、完整审批行、唯一行数、无写入和 Provider 次数，不是削弱产品断言。事务注入反例另证明终态保存不能写回拒绝组。

NEXT17-R01 的 start 失败、resolve 失败和最终 runtime 销毁均由独立子进程验证：catch_unwind 不逃逸、析构次数为一、合成 sentinel 不出 stdout/stderr；普通 Host panic hook 正控仍保留。结合 B 受审实现，R01 在本动态范围关闭。可信同步本机边界不扩大为恶意插件隔离或无限回调中止保证。

A10 代码使用真实 Engine 子进程、kill+wait、同库独立恢复进程、显式 resume/unknown 拒绝；原现场在启动前取消自动删除，OwnedChild 持有句柄负责回收。经理进一步只读查询 attempt3 的 15 份阶段 SQLite 快照，integrity_check 全部 ok：四正常阶段在两次恢复中保留相同结果，无审批；已批准写入从 claimed 转为 unknown，任务 interrupted，第二次恢复仍保持，不伪造完成。记录见[故障快照核验](../../../项目经理/审查记录/2026-10-31NEXT-17-D故障快照核验.json)。日志和测试断言同时检查调用次数及目标文件未产生；数据库检查本身不单独证明进程动作。

## 剩余门禁

NEXT17-R02 由 B 最小别名修订 `a95e9923` 承接，静态 diff 已审，但本 D 候选没有该修复，不能改写为 Clippy 通过。经理已将完整组合交 C 最终五门禁；A11/A12 接口组件另有 C 审查，A13/A14 和主线整合尚待。D 当前源码冻结/no-build，原失败与现场保留。
