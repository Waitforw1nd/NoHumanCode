# NEXT-18 C 组件审查与最终五门禁窗口

2026-10-31，项目经理。APPROVED（公开组件范围）；C18-R04/R05动态闭合。NEXT-18尚未联合验收或合入main。

经理实读[针对性报告](../../提交报告/第十九轮/NEXT-18针对性组件attempt2报告.md)与原始integration.log/meta；固定HEAD `7fce5b2e187a1147f4fcc784b5d07580eb6afe2c`，NoManCode `da5787dfff470eda857fff8c7934432ba5fc8cc3`，Rust `b5846dcd4c4a8584d2b36fe0a2c95e688c2fbf83`。all-targets check、显式CLI build、两integration、限定Clippy及fmt均exit0；CLI6/0/0、workflow7/0/0。

R04旧任务完整成功及最后启用revision断言执行通过；R05两处真实TCP发送端分别记录HTTP200头、声明100/实际1字节、shutdown成功及CLI退出前无额外连接，真实Host代理保存提交结果后再截断，随后同key原receipt回放通过。原hyper任务panic未再出现在本轮integration日志，未夸大为独立客户端抓包证据。

经理独立核17对原/发布hash与长度，6份仅路径转换；10份168项清单相同并匹配固定Git检出及工作树（164原blob/4按gitattributes检出CRLF的cmd），五步版本/起止clean/退出相符。manifest SHA256 `80646D3738230B773325954F819E4169DD0E1ABFBE31939EF55EA9D8DDDDDD4E`，详见[经理核验](../../../项目经理/审查记录/2026-10-31NEXT-18-C组件attempt2原件核验.json)。先前HTTP1/0/0和D组件的准确版本归属保持，不计为本轮复跑；C本人已释放窗口并持续冻结。

## 最终完整组合窗口

原C独占，B/D继续source/index/no-build冻结。C本人重新核收上述三SHA、分支、status/index/unmerged空及无重型进程。全程固定源码/index；先显式执行 `cargo build --offline --locked --bin peachsh` 并记录同target真实CLI哈希，作为W08等使用CLI的前置；它不是五门禁之一。

随后在原C树的 `NoManCode/rust-app` 依序执行，首失败停止：

1. `cargo check --offline --locked --workspace --all-targets`
2. `cargo test --offline --locked --workspace -- --nocapture`
3. `cargo fmt --all -- --check`
4. `cargo clippy --offline --locked --workspace --all-targets -- -D warnings`
5. `cargo check --offline --locked -p peachsh-ui --target wasm32-unknown-unknown`

直接Cargo命令沿用既有MSVC/SDK环境，避免build.ps1的release构建分支。本次增加nocapture只用于保留阶段目录映射及真实失败输出，不改变测试并发、timeout或断言；普通panic hook输出应按实际测试上下文解释，不能将所有panic文本一概当失败或一概忽略。付费live保持原ignored，不加ignored执行。

工程/target/TMP/TEMP/log均原E树，独立最终日志与临时现场；已有缓存可复用，工具链只读，offline/locked、dev/test debug0、incremental0、jobs2，旧fixture的rust-app/target前置保持。不安装升级、不清理旧现场、不改权限或源码、不release/live/push。

每步前后记录完整源码清单、三SHA/clean、准确命令/UTC/退出；派生统计必须来自原日志，完整workspace结果不叠加此前组件。保留first_user_workflow及所有兼容回归的真实结果；不是只跑NEXT-18测试。

新13个runtime迁移/管理意图崩溃现场按日志阶段目录记录，保留原DB/WAL/SHM（存在者）、phase/PID/signal/kill+wait、queryable快照及两次不同进程重开证据。W08/A10等已有现场也按实际可归属证据保留，未知目录不猜测归属；不得打开原始崩溃数据库造成恢复，语义核验仅使用独立queryable快照。不改测试实现来补本轮报告。

失败时原件与首失败回报后由原owner静态修订，不继续余下门禁。成功后发布逐项原/副本路径映射、字节和hash、阶段索引、准确suite计数、未执行事项及本人释放/冻结回执。经理独立核M01–M16及证据版本后才允许main整合；本窗口不授权C自行合main或宣布全产品完成。
