# NEXT-18 C 完整组合公开组件窗口

2026-10-31，项目经理。原C-R19继续；原D独立组件通过并本人释放窗口，B/D冻结。经理收到C本人持续source/index冻结及clean后，仅快进D完整组合到C，C原五个owner文件blob全部保持。

固定HEAD `283a29858b914c1f26318f3a9603739ebc46be1d`；NoManCode `88d9baeda5df6996b3b3b48957aaea60bcd523c7`；Rust `12451bb973c48cf2bbbeade0c89fd5a776c905e6`。整合记录见[经理核验](../../../项目经理/审查记录/2026-10-31NEXT-18-D到C完整组合整合.json)。

C本人先核收三SHA、分支、status/index/unmerged空、无重型进程；满足后可在唯一C窗口顺序执行，首失败停止：

1. `cargo check --offline --locked --all-targets`
2. `cargo build --offline --locked --bin peachsh`，核本target的真实CLI存在并记录hash。
3. `cargo test --offline --locked --lib runtime_management_http_tests -- --nocapture`，须记录实际选中数，零选中不算通过。
4. `cargo test --offline --locked --test runtime_management_cli --test runtime_management_workflow -- --nocapture`

本窗口验证新的公开HTTP契约、CLI恶意/未知结果处理、真实Host重启和管理工作流；原C18-R01–R03需用真实结果关闭，不能用静态报告替代。范围覆盖M04/M06/M07/M11–M14的公开部分，不单独证明M01–M16全矩阵。

所有工程、target、TMP/TEMP、driver/log均在原E树；offline/locked、debug0/incremental0/jobs2、既有工具链只读，准备旧fixture需要的rust-app/target。不安装、清理、改测试并发/timeout/断言，不执行付费live或发布。源码/index冻结；每条命令记录UTC/退出/前后源码清单、完整三SHA及CLI前置，原件与路径相对化副本逐项hash绑定。

失败后保全原件并报告具体断言与实际输入，由原owner限定修订，不在运行窗口直接改源码。成功后释放窗口并提交原/发布映射，由经理审查后放行固定完整组合最终五门禁。最终必须完整workspace check/test、fmt、all-targets Clippy `-D warnings`、peachsh-ui wasm32检查，持续保留first_user_workflow及全部兼容回归；本窗口不替代最终门禁或main验收。
