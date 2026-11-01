# Host CPU 候选 W05 隐藏启动最小增量

2026-11-02，员工 B。按经理 `2026-11-02HostCPU固定候选审查` 接续，基线 `3e5ac1bd1db9b32d43f15f0bcafdd95de93684e9`，产品 tree `01ad4dd9e1a7f6f247a57e2c873f957e8b0a990b`、Rust tree `ee180ebc2d31e6d32ea516b771ad730dff691e13`。当前 B/C 均静态，无动态窗口。

W05 父测试对同一 lib-test helper 的 `Command` 显式设置 Windows `CREATE_NO_WINDOW`；stdin/out/err 管道、参数、原对象观察和无额外 Job 持有均保持。未全局更改其他创建标志。`io_loop` 的第8个参数仅 test cfg 用于最终门响应验证，局部 `cfg_attr(test, allow(clippy::too_many_arguments))` 标明原因；未运行 Clippy，不将潜在 lint 记为实测失败。

固定增量 `f898f0b372bada37e2b962890c3dca23935c2f40`，产品 tree `77d0ecdf81b4ad90029bbc85db06271cc4478276`、Rust tree `43a9b604fb5e21b111cc831d98d2a92794d0962f`。独立树 clean/index 空、gate absent；相对基线仅 `src/wasm_worker_process.rs` 和 `src/wasm_worker_process_tests.rs`。两文件 rustfmt/check、提交前后 diff-check 通过；未编译、运行测试、helper/probe/worker、Clippy 或其他动态。上一份 CPU 报告已追加双方均静态及经理核97原件双hash零差异的更正。下一步经理审增量并决定21 exact窗口，不自行启动。
