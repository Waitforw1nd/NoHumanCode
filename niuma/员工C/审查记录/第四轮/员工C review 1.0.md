# C-R4-01 review 1.0 — CHANGES_REQUIRED

2026-09-26，经理 GPT-6。固定候选 `5bc5e0f4128b512009cb46e3014076f7d0069aeb`，开发基线 `e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`；唯一三处源码范围正确。C回报check与定向cli 9/9通过，五门禁尚在执行；本review不把未收到日志的结果当经理实跑。

经理及A对照[正式契约](../../任务/第四轮/审批CLI契约.md)与[补充1](../../任务/第四轮/审批CLI契约补充1.md)发现两项阻断，已直接交C返工；维持原任务/工作树/分支，新增commit，不改写候选历史。

## C-R4-F1：响应形状必须由命令确定

`src/cli.rs::normalize_success` 依据响应是否存在approvals字段决定列表/单卡，所有四命令都共用。因而approval get/approve/deny收到200 `{"approvals":[]}`也会成功exit0。这没有验证被请求单卡的DTO，并可误报决定成功。

修法：在命令分派处固定List/Single期望形状，分别完整反序列化；单卡不允许列表冒充，列表不允许单卡冒充。测试从真实二进制+mock分别触发两方向，错误exit1/stdout空/固定安全stderr，POST仍最多一次。

## C-R4-F2：已知token必须检查解码后的字符串

`decide`的成功分支使用`value.to_string().contains(token)`。bootstrap token若含HTTP header可允许的引号或反斜杠，序列化JSON加入转义后不再包含原token字面值；但白名单DTO字符串仍含decoded token，可被输出。错误分支只检查最终Failure.message，遗漏契约要求“任意响应字段回显token均拒绝”的额外字段情形。

修法：保留token上下文，在反序列化到白名单、丢弃未知字段之前递归检查decoded Value字符串（对象键若含token也拒绝）；成功和错误响应统一执行。不得仅扫描原字节或JSON重序列化文本。补引号/反斜杠token、嵌套/额外字段、合法ErrorBody及人读/JSON输出测试；固定拒绝、token不输出、不重试POST。

## 补证检查

最终版本核对CL05真实非法ID分支：当前`..`用例还带data-dir，可能先在参数冲突处分流；应单独证明dot/dotdot/控制字符/129字节在网络前拒绝，合法128与斜杠等编码行为有目标请求路径证据。CL03应在隔离temp父目录下设置cwd，避免检测共享系统temp的../data-rust；用持有Host实例锁的实际运行服务证明CLI不争锁，保留零本地DB/文件断言。此为证据加强，不扩产品范围。

C完成后提交新完整SHA并更新报告/身份，A差异复核，两项阻断闭合且五门禁通过才验收。未合并/推送/发布。
