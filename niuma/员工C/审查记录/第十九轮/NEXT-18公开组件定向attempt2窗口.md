# NEXT-18 C 公开组件定向 attempt2 窗口

2026-10-31，项目经理。C18-R04/R05沿原C-R19接续。经理已完整读7cf365及7fce5b2两次增量，后者恢复完整token检查、使用oneshot在CLI进程真实退出后通知服务端，biased select优先检查额外连接。原500ms观察空缺已静态补齐，原终帧修订保持；可进入动态验证，尚不认定问题闭合。

固定HEAD `7fce5b2e187a1147f4fcc784b5d07580eb6afe2c`，NoManCode `da5787dfff470eda857fff8c7934432ba5fc8cc3`，Rust `b5846dcd4c4a8584d2b36fe0a2c95e688c2fbf83`。相对283a298仅两个C集成测试文件改变，生产和库输入未变；经理已核clean。

原C唯一重型窗口，B/D冻结。本人核收三SHA/status/index/unmerged空及无重型进程后，按序执行，首失败停止：

1. `cargo check --offline --locked --all-targets`
2. `cargo build --offline --locked --bin peachsh`，记录实际CLI hash。
3. `cargo test --offline --locked --test runtime_management_cli --test runtime_management_workflow -- --nocapture`
4. `cargo clippy --offline --locked --test runtime_management_cli --test runtime_management_workflow -- -D warnings`
5. `cargo fmt --all -- --check`

验证旧排空用例必须完整成功并到达最后启用revision断言；两处TCP夹具必须记录实际写出的响应头/1字节body/shutdown及CLI退出前无额外连接。发送端写入成功不夸大为独立抓包或对端逐字节观测；检查不再依靠hyper任务panic。

原E工程/target/TMP/TEMP/log、offline/locked、debug0/incremental0/jobs2和工具链只读保持，准备旧fixture target。源码/index冻结；每步前后完整源码清单、命令/UTC/退出及原/发布映射保留，原失败不覆盖。不改timeout、并发、有效断言或生产代码。

未改HTTP的1/0/0仍归属283a298；D的314/0/0库运行仍归属896b690。本窗口不重跑这些未变输入，不冒称在新HEAD执行。完成后本人释放窗口，由经理审原件，再单独放行固定完整组合最终五门禁及全回归。主线仍NEXT-17。
