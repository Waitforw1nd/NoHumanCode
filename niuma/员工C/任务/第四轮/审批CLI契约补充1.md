# C-R4-01 契约补充1：响应验证与凭据回显

2026-09-26，经理 GPT-6。补充原CL06/CL09，不扩大三处源码写权。

CLI采用有限响应读取（上限1MiB）；不仅检查id/status/execution三个字段，应按NEXT-02D已验收Approval安全DTO的完整字段/类型/枚举解析。列表与单卡按实际命令分别验证，不能把错误形状当成功。输出只含白名单DTO，拒绝或丢弃未声明字段；无效响应使用固定安全错误，不输出原body。

decision持有bootstrap token期间，任何成功/失败响应都不得把该已知token带入stdout/stderr。若响应在任意字段回显token，允许将其视为无效响应、固定安全错误；正常Host ErrorBody原样保留code/message/retryable/error。增加本机mock在合法错误message与成功DTO/额外字段中回显token的子进程测试，断言token不输出、POST不重试。bootstrap错误不作为正常审批错误展示其任意正文，保持原安全要求。

参数拒绝不得因clap默认复述输入而泄露已被既有敏感ID校验拒绝的token样式字符串；新增此拒绝的输出断言。所有安全响应处理仍不能依赖启发式过滤识别未知任意秘密；报告准确界定已知bootstrap token与已识别凭据的保护边界。

## A契约审查裁决

- bootstrap token仅复用既有浏览器跨站防护，不提供同机进程认证；不改Host安全架构。
- 子命令显式与data-dir/legacy-data/workspace服务路径参数冲突，exit2且无文件副作用；默认路径不算显式提供。port=0拒绝。port/json接受在子命令前，help按实际支持位置写，不要求扩展所有位置。
- 固定connect timeout 2秒、总timeout 4秒。decision POST的网络失败/超时保守输出本地code `decision_result_unknown`、retryable=false、固定“审批决定结果未知，请用 approval get 查询”，exit1；绝不自动重试。bootstrap/读取阶段失败可用unavailable，不假称决定未生效。
- 本地JSON失败字段固定code/message/retryable/error，error等于message；参数语法exit2遵循clap安全文案，不承诺其为JSON。正规HTTP ErrorBody沿原契约忠实转交（不按中文猜分类）；已知token及可识别秘密回显拒绝为固定invalid response。成功DTO未知字段丢弃，完整字段/类型校验，不能回传任意Value。
- ID直接复用公开validate_persisted_id；路径分段编码。`/ ? # %2f`等不一定违反共享ID契约，合法ID应被安全编码并得到相应404，不强行声明这些字符全为非法；dot/dotdot不得被URL库规范化成不同端点，明确拒绝或安全处理。非法长度/控制/敏感样式在网络前拒绝。
- 无等待者只是测试布置，CLI永远不能自己推断worker存在或显示“已执行”。
