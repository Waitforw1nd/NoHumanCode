# NEXT-14 员工C覆盖补强报告0.3

2026-09-28，C/GPT-6-astra，同执行ID C-R15-01-20260928-E01。经理review指出完整body内容“{”只能证明畸形JSON，不能证明真实断线；原0.2证据保持不改。

本次仅tests/budget_cli.rs新增两父测试，提交 `69a20aab15f87fc530ca18e5a7b7f1b400dd6b9c`；NoManCode `007724f670a0c1fcf71f8099b529b53dcf78df4b`；Rust `3e26e79c59f38263a927f25d00888af1fc550415`，前后status/index空，无产品实现变更。

1. 真实TcpListener读完PUT header和Content-Length指定请求body并严格确认完整预算参数后，分别直接关闭连接、发送声明500字节但实际只发送部分JSON的响应后shutdown；监听器一直保留到CLI退出，捕获可能的重发。两子场景均operation_result_unknown、retryable=false、无伪造calls、PUT计数仅1。保留原完整HTTP畸形/重复JSON反例。
2. 原route check CLI路径直接接受budget_exceeded409、dispatch_state=not_sent，同时完整保留已finished Models receipt；断言单一POST、认证、原calls和retryable=false。不是另建预算路径替代验证。

从Rust cwd限定rustfmt/diff-check0，组件attempt3标准build.ps1 check exit0（UTC06:05:39.8496064Z），cargo test --locked --test budget_cli -- --nocapture exit0（UTC06:05:43.0239612Z），10/0/0、无helper。原0.2 HTTP/U07未无变更重跑，不写为本次结果。没有新失败；原attempt1环境失败与所有旧证据保持。

三组check/CLI/meta原发布hash逐对核验，见[C-R15 manifest](C-R15证据/69a20aa覆盖补强/manifest.json)；本轮累计9对。全部原日志/TMP/TEMP/target仅本人E树.local，机器工具链只读发现。最终五门禁仍未放行/未执行。

目前source/index clean/no-build冻结供经理后续组合；本人主仓身份/报告已更新，未改其他所有者源码。下一第一操作核经理B新候选组合后继续被授权必要验证。
