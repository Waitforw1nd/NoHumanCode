# C-R9-01 开发修订review 0.2

2026-09-27，经理GPT-6。固定受审6410b2f836d516efb1b6b938b52311bfbb7b9796；本次完整核新增server/cli差异与project_http拒绝反例、project_cli四项及实际app.js行为测试代码，不是最终验收。

N08-C01：配置嵌套必须对象且全部五字段；RunRequest与Task拒绝未知字段与位置数组，严格原始JSON检查覆盖重复键。测试通过真实HTTP发送config/request/Task数组、未知project_id、重复tools/prompt，确认配置不变、无Run、零网络。CLI配置文件另检完整对象，重复字段仍由typed反序列化拒绝，零管理请求。该项限定源码闭合，员工报告HTTP5/5、CLI4/4，日志hash与最终组合待核。

N08-C02：HTTP WASM list直接409固定project_operation_unsupported，完全移除list路径解析与调用；run同样明确拒绝。真实HTTP反例含坏manifest、有/无项目、缺失项目，都返回unsupported；源码不调用wasm读取，UI说明未接入。限定源码闭合，不声称内部插件已满足项目权限。

其他：原NetworkError仍经classify映射，CLI支持project错误且保留call receipt；project_cli真实子进程验证finished call原样保留。Web测试运行实际app.js和显式DOM/fetch mock，四组行为检查通过为员工报告，不是浏览器渲染验证。真Host+CLI正向另在project_http。

当前network_http3/5：两项真进程crash setup尚未适配schema11；原失败不删除，C继续修真实项目链。旧其余定向及固定五门禁未完成。因此结论为开发修订可继续组合，C任务仍进行中，非APPROVED。

经理核该保存点clean后合B a7修订，得到e2f965e43a3a679850849f1e5fec1e85a7e1928a（NoManCode ab2737c9df84920832d321c3939120b83a8b1a72，Rust e1e5e85a845b2804169967d8c1a8d3d943cb4d42），merge0无冲突、status/index空。后续证据绑定新组合及其增量。

证据补核：经理实读阶段02manifest，17对原始/发布文件SHA256全部一致；manifest SHA256为08d5cc25a1c16ececac38a20d391872330e313b6473ffd77a8ae7c66b550cb7b。日志独立核得project_cli4/0/0、project_http5/0/0、network_cli7/0/0、network_http3/2/0，以及Web四组PASS；没有把失败或重复运行累加成全量结果。原始文件在C独立树.local/review-evidence，发布副本见本人阶段02报告。
