# integrated-static-2 自定义静态计划人工审查

2026-11-16，D，只读经理整合树。完整读取三个日志，核验manifest/started/result/direct-exit、候选Git对象及前后源码快照；未运行Cargo或产品进程。29项一致性检查均相符，详 `integrated-static-2人工复核/D核验清单.json`。该目录另保存15件原attempt文件及经理closure/gate的原字节副本，共17件；SHA256/长度逐件列明，没有归一化日志换行。

只绑定受测HEAD `9eaf71425d26d0da25b093e852a702262bcaa164`，产品tree `37319fbee48ed5037ba140a325c467b06db58072`、Rust tree `927dc61d107caa9996c6a8628ab055ffa2c57362`。复核时integration已到 `5f2430b295564a3f6ae9c105f3ca7c860c0d8c35` 且双tree改变，不能把本次静态结果转授当前版本。

| 顺序 | 实际命令 | 步骤/原直接进程退出码 | 完整日志字节 / SHA256 |
| --- | --- | --- | --- |
| 1 | cargo clippy --workspace --all-targets --offline --locked -- -D warnings | 0 / 0 | 805 / cb9e33f2d3f88f0d3188fed4557da224933e24c6c8f2bbc6c83beb0edea3fbf3 |
| 2 | cargo fmt --all -- --check | 0 / 0 | 175 / b814b64e464df769258ec75fbda9ea3805547d98a2cbec70a95c35107b28fb46 |
| 3 | cargo check -p peachsh-ui --target wasm32-unknown-unknown --offline --locked | 0 / 0 | 712 / 6307f9583eb3b419a66d6b69aca74fb34d2d348a8ee6d08aee720be663a9a789 |

manifest外层runner_exit_code=0、status=passed、inputs_verified=true。三个步骤串行时间顺序一致，均terminal_unknown=false、1800秒deadline。14件manifest工件的hash/长度全部相符，无额外或缺失attempt文件；step/result/started和实际枚举命令一致。source-before/after原字节相同（SHA256 `5a5583d3f4758250fdedd59815610a18922fe2e4bc0380638545e0f5e4897a04`），记录clean=true、空status；重算文件清单source_sha256及Git双tree相符。

Clippy完整日志显示protocol/Host/UI检查及Finished；fmt日志只有环境发现行，结合实际fmt --check命令和原退出0判定该门通过；wasm-check显示protocol/UI检查及Finished。Clippy、wasm-check均含proc-macro-error2 v2.0.1未来Rust兼容警告；不称零警告、不声称未来工具链保证，当前 -D warnings 命令仍退出0。

环境记录含E盘target/TMP/TEMP、offline、jobs2、dev/test debug0、incremental0、testthreads1。旧manifest的MSVC/SDK/CC/AR字段为null是Windows字典键大小写记录缺陷；三个原日志明确打印现有MSVC14.43.34808的Hostx86/x64 cl.exe及SDK10.0.22621.0。旧manifest不补写，后续修复db4042e3另行验收。已安装工具本身位于系统盘，与活动输出E盘要求区分。

经理closure记录原监督会话92681、退出0、无unknown、源码不变；其gate_sha256与归档锁原字节相符。该closure是经理的原会话收尾记录，D没有新增进程树证明，也未按PID快照判定清理。

计划mode为manager-static-diagnostics，明确required_order=clippy/fmt/wasm-check。通用publisher只接受枚举标准计划，不能以改写mode、补造test_targets等字段让此自定义计划冒充full通过。本人工清单只证明三项静态计划证据自洽，不包含Rust测试/网页/debug bins，不改判integrated-full-1的失败，不验收W3或新provider修复。
