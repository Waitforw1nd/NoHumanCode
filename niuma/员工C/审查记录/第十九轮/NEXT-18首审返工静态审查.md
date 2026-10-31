# NEXT-18 C 首审返工静态审查

2026-10-31，项目经理。固定候选 `273895d09908ddc5a7eb0ac6152d3a7494b77e66`，NoManCode `fb7313223c47450274e11db02a5f68f854d499a7`，Rust `816c5d24c7aef27fcd98ba071f8e210c546e8efb`。经理实核 HEAD/status，已读相对4b47首候选的完整四文件增量、真实Host夹具及相关现有CLI/服务端投影。

结论：静态返工可进入依赖整合准备；本次未发现新的阻断性静态问题。不是功能APPROVED，不解除no-build，不认定M项通过。B仍为活动草稿，不能合入；待B固定候选与双方冻结后由经理整合，并核真实状态机与接口后安排唯一组件窗口。

## 首审意见处理

- C18-R01：新增两类transition关闭admission校验，Agent排空/active/transition关系及effects约束；正反例包括非法写响应unknown、合法Agent draining、Provider禁用后仍in-flight、transition旧effects和完整u64。与当前B草稿的start/stop预留行为一致，仍需固定B版本联合核对。
- C18-R02：新增独立禁用两组件时真实Task失败、原创建key回放、无Completion请求/调用账本/预算增长，以及各自恢复执行；真实在途Agent排空、Busy不消费key和旧receipt回放；四插件key冲突、CAS、同态新revision与回放代次不变。新增代理完整读取请求并转发真实Host，保存真实提交结果后截断响应，随后显式同key回放。已从“仅handler入口计数”推进到可验证实际提交的测试代码。
- C18-R03：新增HTTP方法/HEAD/Host/body/header/revision/content-type边界，以及CLI嵌套重复字段、超限、不匹配错误和参数出口；Models按call_id查询持久回执，Balance另建真实账户/授权路径。核CLI run投影确有tools/write_scopes字段，新增无工具断言不是引用不存在字段。

## 动态阶段必须保留的证明

实际运行新Host/CLI/代理与余额夹具，核结果、单次发送、原receipt和请求计数；不能仅凭本次静态阅读关闭M04/M06/M12–M14。客户端状态约束需接受B实际合法快照，同时拒绝非法快照。组件结果只对其固定源码组合有效，最终M15/M16仍须完整回归。

首候选报告与审查保留。C继续source/index冻结、no-build；经理未修改C产品文件、未运行产品测试。
