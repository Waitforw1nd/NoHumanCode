# 员工 C review 1.0：单 Agent CLI 贯通

经理 GPT-6，2026-09-27。C-R6-01 / C-R6-01-20260927-000811，实际实现/正式验证模型 GPT-6-astra。结论：**APPROVED（NEXT-05 v1.0 的 C 实现及固定联合验证范围）**。受审候选 `1a3f3e0840331d54e3419b5826b845d3667feed0`，Rust tree `ba569b4e5c4365d5e71a3075ecc7fafe48373bfa`。

已读[正式报告](<../../提交报告/第六轮/员工C（单Agent CLI贯通 第1.0轮报告）.md>)、[34份双哈希清单](../../提交报告/第六轮/C-R6证据/manifest.json)、固定CLI/server/main与新增测试差异、最后两个测试文件修改及原始门禁日志。经理核34对原始/发布哈希相符，五门禁命令全部绑定同一完整候选；员工声明源码与档案停止写入，工作树干净。B/D按各自review限定通过，联合结论见[经理验收](../../../项目经理/审查记录/2026-09-27NEXT-05联合验收.md)。

## 实现审查

1. CLI是既有Host客户端：固定loopback，no_proxy/no_redirect、普通1MiB上限与短超时、写前bootstrap；未复制Store业务或在客户端创建数据库。start默认禁工具/命令，无默认广泛scope，身份通过真实context读取；create成功不依赖后续context查询。
2. status仅投影必要状态，changes/restore反序列化安全DTO并核关系；partial/unknown回执保留逐路径事实，exit1，不把业务恢复失败丢成无内容错误。不可验证的POST回复返回结果未知，无自动重发。服务token在解码后递归检查，避免转义回显。
3. SSE与普通请求分开：无4秒整流超时，有连接/停滞/帧大小边界，严格cursor和事件关联；输出flush后推进游标，错误/EOF带最后cursor。手动游标续传不触发写操作重试。
4. context直接调用B typed API；NotFound/Unmapped/CorruptState及UnresolvedEffects映射安全状态码，不泄漏内部来源。main改动仅扩展CLI参数错误提示。
5. 旧Turn HTTP工具排除升级为依赖/完整来源检查；当前坏消息400和继承前缀损坏500分别验证零对象/零Provider增量，保留旧安全目标及空工具数组正例。

## 验证与问题闭合

- 完整CLI/Host9项、原审批CLI11项、Turn HTTP17项通过。真实进程kill并wait后重启同data-dir，继续原Session；未知恢复结果仍阻断新key，不会重做效果。mock Provider是本地确定性替身，不冒称付费模型实跑。
- N02批准命令已核真实tool_result.exit_code=0和产物字节，不以模型completed替代。首轮命令位置参数夹具未产物的失败、诊断日志保留；只改测试为显式PowerShell参数，不降低断言或修改生产执行。
- 首轮安全字符串误用scrub折叠正常多行内容，改为不损失内容的redact_persisted安全比较；FK夹具和Clippy失败均保留。旧400断言因继承完整性500先拦截而失败，已分离来源与格式场景。
- 最终固定候选五标准命令check/test/fmt/clippy/wasm-check全exit0，303通过/0失败/1付费live忽略。fmt无输出、Tee未建文件，仅命令账记录真实退出码/时间，不伪造空日志。
- Ctrl+C独立系统信号未注入；源码路径只终止观察、无cancel调用，真实limit断开后任务仍running为已跑证据。该明确边界非本片阻断，不称信号实测。上游proc-macro-error2未来兼容警告保留。

经理文档核验首次发现C历史身份后缀差一个CR字节（规范换行后文字相同），已仅保留本轮新前缀并恢复旧后缀原字节；不是产品测试失败。未改产品源码/重跑员工正式测试，未发布或付费live。无待修产品阻断，可由经理串行整合此固定组合；本结论不等于阶段一整体完成。
