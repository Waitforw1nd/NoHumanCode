# NEXT-21W2 基础窗口 attempt3（员工 B，2026-11-01）

## 固定对象和范围

独立工作树 `../nhc-b-wasm-worker-process`，分支 `codex/b/wasm-worker-process`，原任务基线 `7dd3728f58985244dbe09f5486e4568e94bde408`；本次候选 `8937aed18124c4db75c115b9849797b49fd3c51d`，产品树 `71ca16bffb89c41e5cf5f34ed108e8b6657b6196`，Rust 树 `58059e6805ca324d1555023d8501286fd26d9097`。源码写入和提交由 B 执行，整合由经理负责。独立树前后 clean；主仓人员文件未由 B 暂存或提交。

本提交修复 attempt2 的两处 Clippy 问题；管道成功零字节读继续读取，broken pipe 才计 EOF。新增 W02 测试和零字节 WriteFile 后附尾字节的独立故障 probe；W02 本次仅通过 all-targets 编译检查、未运行，不宣称动态触发零读。该组件依赖可信 worker 和无 imports WASM，不是任意原生插件的操作系统文件或网络隔离。

## 本次结果

日志和快照位于独立树 `NoManCode/rust-app/.local/next21w2-b-8937aed1-attempt3/`。固定身份/clean/重型进程预检后，按首败停止：workspace all-targets check、lib no-run、真实 worker build、四个 gate exact、W01 exact、fmt check、workspace all-targets Clippy `-D warnings` 全部 exit 0。四个 gate 分别覆盖重复 poll、取消与接受竞争、后台观察取消、预取消不启动 supervisor；每项单线程。

W01 before PID 49052，8,388,544 字节模块；after PID 39848。两个均 exit 0、进程 signaled、Job active 0、I/O 完成；阶段日志记录各自挂起创建、恢复和回收。build 后从本次 target 的 `debug/peachsh-wasm-worker.exe` 拷至本 attempt 的 `artifact/`，源和快照 SHA-256 均为 `d759be294bb798c4651b558fbf8b32c6cbf52ae4d657e4899eecdface8454821`；结束快照摘要一致。前后源码清单文件 SHA-256 同为 `df18491924279982e337122ed8d6b8e8dadd7504e8289635678492e4ee6fdafb`；收尾无重型进程，无 cleanup_unknown 阶段。

前一候选 attempt2 的四 gate 和 W01 通过，但 Clippy 失败（参数数目和可折叠 if），结果和快照保留在 `NoManCode/rust-app/.local/next21w2-b-c63d7da2-attempt2/`；attempt1 在 cargo 前预检停止。本次新提交及新 attempt 不覆盖其证据。

## 未完成和下一步

W02–W06 未运行、未验收；W02 即使将来通过，也须分别报告机器是否实际观察到 `stdout_zero_read`，不得将尾字节被拒绝等同于零读动态命中。真实 Host 强杀、W3 持久恢复、安装信任根、严格全过程硬时限及 A/P08 真实 profile 可写反例均未闭合。本报告仅交经理审查基础窗口，不代表完整 W2 通过或产品接入。下一步由经理审查此候选与证据，并按后续授权执行 W02–W06 独立窗口。
