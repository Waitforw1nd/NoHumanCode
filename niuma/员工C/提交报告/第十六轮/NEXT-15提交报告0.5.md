# NEXT-15 员工C报告0.5：最终门禁首失败与单次W08诊断

更新日期：2026-09-29北京时间；以下执行时刻均为2026-09-28 UTC。员工C / GPT-6-astra；执行ID C-R16-01-20260928-E01。依据经理2026-09-28最终五门禁正式放行、后续一次既有binary精确诊断授权。源树为仓库根相对 ../nhc-c-host-setup-cli，分支codex/c/host-setup-cli。

**结论：最终check通过，test失败，后三门禁未执行。单次精确诊断通过但未复现原超时，根因未确定，不得将其改判为完整门禁通过。没有改源码、断言、时限或并发设置；没有连续重跑至绿。**

## 固定受测版本及环境

- HEAD：fa9623785573a8f44decd0368fa73a4e267f4ed6。
- NoManCode tree：2ec3a9f19c9a0f01721581d3cd0a7752187ae522。
- Rust tree：1fcf1e19dcda993dd1778a37e50e613e971d5153。
- check/test每步前后三SHA、分支、status/index/unmerged均核对一致且干净；154个NoManCode跟踪文件的四份前后SHA256清单完全相同，清单SHA256为7358ED10088435B079E31938673B4545977C1E21505423471F6B64ABACA6C1B1。诊断后再次逐文件核154项未变。
- 既有rustc1.98.1、cargo1.98.1、rustfmt1.9.0、MSVC14.43.34808、SDK10.0.22621.0只读；Windows与wasm32-unknown-unknown目标已安装，未安装/更新。
- 原最终驱动.local/next15-final-gates.ps1从自身目录定位Rust cwd，调用现有build.ps1并明确-Action。所有shell显式E目录；TMP/TEMP为本人树.local/tmp，CARGO_TARGET_DIR为.local/target，全部日志及fixture仅E。
- RUSTUP_AUTO_INSTALL=0、CARGO_NET_OFFLINE=true、CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_INCREMENTAL=0、CARGO_BUILD_JOBS=2。原脚本/工具链/环境/源码hash绑定见meta；没有release、live、push或主仓index/merge操作。

## 五门禁实际执行

根目录启动命令：pwsh -NoProfile -File .local/next15-final-gates.ps1 -Attempt attempt1。每个build.ps1动作cwd为NoManCode/rust-app。

| 门禁 | 实际动作及底层原命令 | UTC开始—结束 | 结果 |
|---|---|---|---|
| check | pwsh -NoProfile -File ./build.ps1 -Action check；cargo check --workspace --locked | 15:56:39.8097947Z—15:57:40.0279563Z | exit0 |
| test | pwsh -NoProfile -File ./build.ps1 -Action test；cargo test --workspace --locked | 15:57:40.8712774Z—16:00:51.121908Z | exit101，停止 |
| fmt | 未调用；预定cargo fmt --all -- --check | 未执行 | 不记通过 |
| clippy | 未调用；预定cargo clippy --workspace --all-targets --locked -- -D warnings | 未执行 | 不记通过 |
| wasm-check | 未调用；预定cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked | 未执行 | 不记通过 |

驱动外层工具进程exit1与其捕获的test/build.ps1 exit101分别保留；没有后续gate日志。test原日志只执行到peachsh lib suite，原始合计236 passed / 1 failed / 0 ignored / 0 filtered out，237个入口，42.65秒。剩余workspace单测、integration、doc suites未启动，不能用以前组件数补齐，也没有本轮完整suite总数。

其中6个子进程helper入口单列：credential_input::tests::redirected_process_child、credential_input::windows_console::tests::isolated_console_child、git_worktree::tests::w05_isolated_environment_helper、host_configuration::tests::transaction_child、host_configuration_crash_tests::host_process_child、provider::network_tests::proxy_environment_child。源码均以专用子进程选择环境变量控制业务分支，标准父suite无该变量时空返回；运行驱动没有设置这些选择变量，后续只读环境核验也无NHC_/PEACHSH_变量。扣除这6个辅助入口的统计为230 passed / 1 failed / 0 ignored，不替换Cargo原始236/1/0。付费live suite未启动，不能写成已运行或ignored1；实际已执行suite的ignored为0。

## 首失败可确认事实与证据缺口

失败入口：server::worktree_http_crash_tests::w08_real_router_process_kill_wait_at_three_durable_boundaries，NoManCode/rust-app/src/server.rs:2900，等待10秒返回deterministic child notification: Elapsed(())。

原stdout确认claim停点operation=7a214d4d-4ca6-4bff-b104-de8bc49b3903、child PID48680；原child kill+wait及restart unknown验证完成，没有额外worker/Provider。源码循环顺序claim→created→registered，因此后续超时在created阶段，但共用wait_file同时用于listen-created和ready，原无backtrace/目标名，不能进一步确定是哪一个等待。更不能据超时直接推断产品错误、CPU负载或子进程已退出。

原W08用tempfile::tempdir；panic展开会析构现场。只读检查原E临时目录没有找到对应child-*.log/listen-*；残存另外三个ready属于另一worktree测试目录，未拿来替代。原子进程stdout/stderr、原created PID、在途原HTTP请求结果和原child最终退出码因此不可恢复。代码ChildGuard Drop会尝试终止并等待自身child，但代码意图不冒充原运行中具体kill/reap结果；运行结束后本树进程扫描为空是另外一项已观察事实。

## 经理授权的唯一精确诊断

未经源码修改或重新编译，直接运行原test.log绑定的已有binary，cwd仍NoManCode/rust-app：

    ../../.local/target/debug/deps/peachsh-dc94de63d8d31b10.exe --exact server::worktree_http_crash_tests::w08_real_router_process_kill_wait_at_three_durable_boundaries --nocapture

binary SHA256：AB2E16C07582435F3DF0A435104484C4F458E775DCE5543A00D4E98BF482CE0B。

实际开始16:05:51.5314699Z、结束16:06:05.2588127Z；RUST_BACKTRACE=1，TMP/TEMP隔离到本人E树.local/tmp/next15-w08-diagnostic1。一次且仅一次；原test父进程PID47548。原stdout为1 passed / 0 failed / 0 ignored / 236 filtered out，测试12.94秒，原binary exit0、取证脚本工具exit0。这236 filtered是exact筛选，不能累计成全suite结果。

该次claim/created/registered原child PID分别28624/43440/44272，均到达目标hook、被父进程kill+wait；各阶段restart分别核unknown/unknown/succeeded并kill+wait。HTTP pending至kill后的原is_err断言均保留并通过；测试没有输出具体transport错误分类或原请求body，所以不额外声称得到了这些数据。三次重启的完整PID未全被轮询采到，不能凭样本捏造完整PID表。

旁路观察144条：保存143个文件版本、另1次读取created现场host.sqlite3-shm时IOException明确不记成功副本。总保存2,711,381字节。已保存6个真实child stdout/stderr合并日志（12个版本，内容仅running 1 test）、3个ready及6个listen标记、Git/DB等现场文件版本。观察是独立文件的尽力捕获，不保证SQLite跨文件原子一致性；原fixture在正常完成时也被原测试自身清理，旁路版本仍保留。没有主动删除旧现场或其它任务文件。

诊断结果：本次三个phase通过，原超时未复现、根因未确定；不再重复诊断、不扩大timeout、不改单线程、不跳过测试、不继续fmt/clippy/wasm-check。可确认的缺陷是原W08失败取证会丢现场，并且等待缺少phase/通知类别/child与原请求结果的诊断信息。

## 原件、发布与hash

原件目录为本人树.local/logs/next15-final-attempt1和next15-w08-diagnostic1，另含.local两个取证驱动脚本。发布清单：[NEXT-15证据/0.5/manifest.json](NEXT-15证据/0.5/manifest.json)。

36对原/发布证据逐件记长度和双SHA256，32对原样、4份仅发布路径相对化；143个新诊断现场原始文件版本独立列hash、相对位置和观察时刻。child日志/ready/listen共21份已包含发布对，其余现场版本保留在本人E树原件及索引，不伪造发布副本，也不混充原最终失败现场。四份154文件源码hash清单、前后元数据、原check/test日志、单次诊断stdout/stderr、环境/binaryhash及停止回执全部保留。

manifest SHA256：E144DF08257E3A70D263572BE7C9A2EBEB3EF14393DDAE3222DE69D4DE10B9C7。

之前0.1–0.4及所有失败/原始证据保持。没有自我APPROVED，没有宣称最终门禁或阶段一完成。

## 归档节点与补充05接续

16:08:58.8893640Z再次核fa962378三SHA、status/index/unmerged干净，154源码hash匹配，无本树构建/Host/tests进程；此为0.5封存时的source/index clean/no-build节点。

随后已完整读取经理2026-09-29冻结补充05。该有效指令仅释放src/server.rs既有cfg(test) worktree_http_crash_tests的最小取证修补：保留成功/失败现场、区分phase/listen/ready/PID、原child实际exit/kill/reap、停点等待中安全观察原HTTP提前结果。保留业务断言及10/12/20秒边界，不改生产/B/D或全套并发。本报告封存后才实施；仅静态格式/差异核验和小提交，新增后续报告，不在本窗口动态重跑。经理review后才重新固定组合、从check开始完整五门禁。本人继续本轮协同，原失败不覆盖。