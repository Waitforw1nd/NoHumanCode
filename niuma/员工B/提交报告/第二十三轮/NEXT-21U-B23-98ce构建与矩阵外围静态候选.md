# B23 98ce构建与20 exact外围静态候选

2026-11-02，员工B Jason。98ce PRECHECK [attempt3](NEXT-21U-B23-98ce-PRECHECK-attempt3结果.md) check/Clippy均原handle exit0；本文件仅提交下一阶段外围供审，未运行新脚本、Cargo、probe、测试或Host。固定HEAD `98cec2e441adcb7cd0476c81db075c8f1ce4659e`、产品 `85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e`、Rust `951cff6854e0a8bc0b7cc48d559b735c60256c21`，独立树clean/index空。

两个独立prepared目录均在员工树Rust `.local/`，未创建执行attempt：

| 包 / 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `b23-build-98cec2e4-prepared/GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `b23-build-98cec2e4-prepared/launch.ps1` | 9954 | `dbdc8ad2ddde27f1ba97642294c0a26a5a9fcfd720e252e2cb5503639695d71c` |
| `b23-build-98cec2e4-prepared/supervisor.ps1` | 14534 | `c93aaf06247827738e15835cc6a412b2aebcbccc427976f8573aa7eee5e52ce9` |
| `b23-build-98cec2e4-prepared/manifest.json` | 728 | `5aac39f3d5aaa9f3206a60c09937e0ebb30c70797f4743aa8b2a308de12af2bb` |
| `b23-matrix-98cec2e4-prepared/GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `b23-matrix-98cec2e4-prepared/launch.ps1` | 10377 | `6edc3390ed026626341d0b99ce22094e5a0e8bfe9ef365f3186c54483917c2f5` |
| `b23-matrix-98cec2e4-prepared/supervisor.ps1` | 11515 | `fa611527317f585acd14e63409e29b06cbed809c7e5e8f4b218f0853c6ec52d7` |
| `b23-matrix-98cec2e4-prepared/manifest.json` | 722 | `7f5209418ae59fcc471a03318954c0f6a00b8985e4e389d28f3adfe57ff38e0a` |

构建包继承已审PRECHECK的原对象两层隐藏启动、common gate、E盘私有缓存/临时/target、固定MSVC/SDK及源码/工具/target快照；两步串行、首败停：`cargo build --example process_lifetime_probe --offline --locked --target-dir <target>`，随后 `cargo test --lib --no-run --offline --locked --target-dir <target> --message-format=json`。仅在两步exit0后从Cargo JSON确定唯一lib test EXE，固定probe和test副本，记录源前/源后/副本SHA及字节，写 `build-receipt.json`。不运行EXE；最终source及外围成功核对后才可作为候选构建证据。

矩阵包需另签，入口除三SHA/自身manifest外要求具体BuildDir和经理审定的完整build-receipt SHA；核构建回执、构建窗口/外层完成状态及两EXE原固定副本hash/长度，再在common gate下以原handle隐藏启动现有tracked `tests/support/process_lifetime/run-fixed.ps1`。该runner另外持其工作树局部gate，逐个exact串行执行16 ignored过程例及4 state例，使用固定probe/test的hash，核每例单个passed、源快照及完成数20。矩阵supervisor保留runner原handle，4200秒界、外层5400秒界；inner incident、超时、原handle/退出不可判定时留句柄和gate，迟到signal不自动开窗。任何已知非零亦留common gate待经理核。通过矩阵仍不代替C接线、真实Host和完整U门禁。

四个PowerShell脚本均Parser零错误，C#仅复制未编译。以上是静态准备，存在的程序可执行与OS行为须由另签窗口验证；本包没有实跑证明。common gate只约束参与者，旧W2/C窗口仍由经理全局调度；固定构建回执是关联证据，不代替经理对原构建日志、原进程、源和工件证据的独立核验。需经理先审/签构建，成功且核其回执后再审/签矩阵，不自动连续运行。
