# C-R9-01 开发检查点review 0.1

2026-09-27，经理GPT-6。输入47457d29151f23ffc78e58559ce4adbb1d480572（C明确尚未Rust编译、未测试的消费检查点）；经理随后合B+D到C形成f02641756df6f48910b314ed8614f5295461ab78，C标准check0，首次环境exit1原样保留。不是最终候选验收。

| 编号 | 固定代码事实 | 收口要求 |
| --- | --- | --- |
| N08-C01 | StrictObject只验外层object，config的ProjectConfig可接受位置数组，request的RunRequest无deny_unknown_fields，可忽略另一个project_id；Task亦需严格形状 | C侧嵌套DTO/deserializer验证对象与允许字段，不跨改B domain。真实HTTP嵌套数组/未知project_id/Task数组与未知字段/重复nested键拒绝，零数据库写和零网络；正常Run正对照保持 |
| N08-C02 | WASM list仅核Project根后拼.peachsh/plugins，内部list仍读manifest，不能保证子路径策略 | 经理批准本片list/run都固定409 project_operation_unsupported，不调用内部list/load/run。内部模块不改，不扩完整插件；契约补充1明确覆盖旧可选list |

C已真实接收两项；首批project_http3通过与Web mock DOM/fetch4组通过为员工报告，尚待最终日志清单/固定SHA核验，不能冒称经理重跑或完整五门禁。继续本人精确所有权实现测试，B/D问题交原owner，不自建第二套业务判定。
