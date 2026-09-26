# C-R9-01 CLI fixture修复定向报告

修复提交 `362d6c09daf6ca89f7f28ff97460a0018d5c4f56`，NoManCode tree `03ddb1383e41dff50173794e28f08896be32eb16`，Rust tree `d632533e96b050035bebb77b295a8535bfb87fd0`，status clean。

仅修改 `tests/cli.rs`：cl03 使用真实 live Host 的项目注册/configure/select/grant，通过独立 current-thread runtime 创建 native schema11 Task 与 pending approval，轮询持久记录后关闭 runtime，确保无 waiter；cl04 同样使用真实项目绑定 Task。未注入 sidecar、未改生产代码、未放宽断言。

`cargo test --locked --test cli` exit0，11/11 passed，0 failed。日志与发布副本双 hash 见 [manifest](C-R9证据/cl03-cl04修复manifest.json)。此前最终门禁 test exit101 原始日志继续保留，未覆盖；最终五门禁需经理重新固定组合后从 check 开始。
