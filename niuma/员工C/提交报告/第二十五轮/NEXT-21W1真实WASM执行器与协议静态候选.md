# NEXT-21W1 真实 WASM 执行器与协议静态候选

2026-11-01 · 员工 C / 6.1 Terra。独立树 `codex/c/wasm-worker-core` 基线 `d5b34a5fd6f735697db4ce55c7055c5053b56d45`，开工 clean；基线产品树 `897cc8510c0c56118df04c2c1dbcea9c999e539f`、Rust 树 `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`。静态候选提交 `48232412fd2133716d1cae9a4b30761c332d8f04`；产品树 `22c7e35546d6b9549c9e6ff407cb00d54917bb75`、Rust 树 `437f4b9f5a4d5876e711f8a7fbada91eec7f33d5`；候选后独立树 clean。经理后续文档提交不改变产品基线。

## 实现与协议

新增 `src/wasm_worker_protocol.rs`、`src/wasm_worker_executor.rs`、`src/bin/peachsh-wasm-worker.rs`、`tests/wasm_worker_core.rs`；`src/lib.rs` 仅注册两模块。bin 因仓库 `bin/` 忽略规则显式加入提交。未修改原 wasm、Engine、Agent、PluginHost、Store、schema、CLI/server、B 隔离封装或 Cargo.lock。

请求依次为三个无符号 32 位大端长度前缀帧：manifest JSON（最多 4096 字节）、策略输入 JSON（8192 字节）、原始 WASM（8 MiB）。分配之前检查各帧长度、拒绝零长度，三帧上限加 12 字节前缀即总上限；收完必须等待 stdin EOF，任何尾随字节拒绝。响应为一个最多 512 KiB 的 JSON 帧，读取方也须检查 EOF。同步读不自带截止；W2 Host 必须在发送完后关闭 stdin、总 deadline 覆盖 EOF 等待和启动/执行/回收。截断、非法 JSON/UTF-8、未知字段均拒绝。

manifest 包含协议版本、request ID、32 位十六进制 nonce、binding generation、插件 ID/版本、SHA-256 digest、fuel 和 memory pages；请求和响应关联通过比较完整 manifest。nonce 随机性由未来 Host 生成并保证，worker 只检查编码，不把格式检查当随机证明。输入包含 round/max_rounds 和 before 工具类别、after 无工具、after 工具组的显式枚举；不含消息、路径、凭据、工具参数或自由文本。输出仅允许 use_tools、tool_votes、finish、stop，检查阶段、升序唯一且范围内的索引、投票数量；最多 256 个工具。Manifest 使用 ASCII 有界身份，fuel 至多 5000 万、内存至多 64 页。解析错误输出固定类别；入口遇到无法关联的畸形帧以固定 stderr 错误退出，不伪造身份响应。

执行器对同一模块快照校验长度和 SHA-256，在 worker 内单次 `Module::new`，拒绝 imports，实例化前检查 memory 最大、alloc/run_json 函数签名及导出 table 限额；`StoreLimits` 限制总内存/表/实例，start 前设置 fuel，单次 instantiate/start，再 alloc、run_json、边界检查及严格 typed JSON 解析。未调用旧 `load`/`validate_module`。自身决策验证仍需未来 AgentLease 最后采纳复核；固定 bin 只从 stdin 收帧及往 stdout 回帧，没有参数指定路径、WASI 或 Host imports。纯组件模块标注只允许受信 worker 使用，尚无生产调用。

## 验证与限制

新增测试静态覆盖：真实 WAT 的非平凡 start 与 before/after 结果、错误类型/import/缺少 memory max/超限 table/坏指针/fuel 循环/非法决策；超长声明的记录型 reader、截断、尾随、错代、错版本、非法 UTF-8/JSON、未知字段、阶段/索引/票数和 digest。start 测试检查一次实例内 start 的效果并覆盖非平凡 start；跨多个实例总执行次数目前由执行器的单一实例化调用静态证明，测试本身不能独立观察先前销毁的实例。后续动态窗口须审查并实际执行这些测试，不可将其描述为已通过。

本轮仅执行新文件 `rustfmt --check` 和提交前 `git diff --cached --check`，均通过。**未编译、未运行测试、Host、worker 或任何 probe**；没有动态通过声明。未来串行组件窗口还须 check、新 tests、fmt、Clippy，并处理任何编译/测试发现。

没有实现进程启动、Job/LPAC、OS 隔离、总 deadline、取消/强杀、主机死亡后回收、真实 PluginHost/Engine 绑定或持久选择与恢复。B 的 A 切片尚未通过，不能据无 imports 断言系统隔离。下一步第一操作等待经理静态审查和独占组件运行窗口；若验收发现缺口按经理裁决返工，不自行占用动态资源。
