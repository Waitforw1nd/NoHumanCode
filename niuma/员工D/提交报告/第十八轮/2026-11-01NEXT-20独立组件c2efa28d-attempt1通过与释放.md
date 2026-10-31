# NEXT-20 D 独立组件 c2efa28d attempt1 通过与释放

2026-11-01，员工 D。固定候选完成授权七步独立组件窗口，全部 exit 0：完整 lib 378/0，八套 integration 合计 92/0。该结果是 D 组件窗口，不是 C 公开 CLI 验收、最终 workspace/UI 门禁或 NEXT-20 联合验收；main 仍 NEXT-19。

N20-F04 仅改 `tests/composition_http.rs`：测试连接单事务先删除 files 唯一管理请求再删除父行，各断言 1；前后完整比较其他三域 desired 与回执，并在提交后确认 files 缺行，再执行原 404 和安全断言。原 FK、500、disabled 与鉴权断言保留。经理静态审查无阻断。

固定 HEAD `c2efa28d169af8b75be0ea3c058aea21f430aef9` / NoManCode `58490a8cc47c9d548fbf40dbede5548658a0c024` / Rust `e6a2029e9081ad9ece8820f20b10637e4c6816ab`；D 独立树 `../nhc-d-default-composition`、分支 `codex/d/default-composition`。环境 Rust/Cargo 1.98.1、MSVC 14.43.34808、SDK 10.0.22621.0，offline/locked、jobs2、debug0、incremental0、默认测试并发，使用既有 D target cache 和独立 TEMP/TMP。

| 步骤 | UTC 起止（2026-10-31） | 结果 |
| --- | --- | --- |
| `cargo check --offline --locked --workspace --all-targets` | 20:46:17–20:46:18 | exit 0 |
| `cargo build --offline --locked --bin peachsh` | 20:46:32–20:46:37 | exit 0 |
| `cargo test --offline --locked --lib process_fixture_failure_controls_reap_job -- --nocapture` | 20:46:51–20:47:08 | exit 0，2/0 |
| `cargo test --offline --locked --lib -- --nocapture` | 20:47:21–20:47:55 | exit 0，378/0 |
| 八套 integration | 20:48:10–20:49:27 | exit 0，92/0 |
| `cargo fmt --all -- --check` | 20:49:40–20:49:41 | exit 0 |
| `cargo clippy --offline --locked --workspace --all-targets -- -D warnings` | 20:49:55–20:50:13 | exit 0 |

integration 实际 cargo 顺序及数量：approval_http 16、checkpoint 15、composition_http 1、plugin_http 9、project_http 6、runtime_management_workflow 7、turn_http 17、workspace_changes 21，均零失败。CLI build 产物当时 29,920,256 字节、SHA-256 `8af1254867d8ce2efab7399f0a442957beb43c75476298fba0c98674fafa2e04`；不推断后续进程实际加载字节。

## 契约覆盖和限制

- C01/C02：普通构造的共享资源及真实 gate 测试通过；不以此声明每个外部入口均已逐项证明。
- C03/C05/C09：四域 desired 与重建、缺行及损坏失败；HTTP 绑定、disabled、认证、安全 500 与本次真实缺行 404 测试通过。
- C04/C06：lib 与上述 HTTP 套件内现有生命周期、审批、事务和失败反例通过；结论限已运行用例。
- C07：真实审批写入、关闭文件/shell 后 diff、活动工作区拒绝与 checkpoint/restore 的组件测试通过；既有 checkpoint 集成 15 项通过。
- C08：普通与 recovered 构造的恢复差异测试通过；该测试不是公开 Host 实际重启证明。
- C10/C11：本窗口运行现有回归和部分项目 CLI 用例；C 的公开 CLI/新用户故事及最终联合验收尚未执行。
- C12：本窗口七步均通过，但最终完整 workspace、UI wasm 与公开验收不由本窗口替代。

身份通知负控、A10/A11/N19、N13 和 W08 真实进程测试本次均为 ok；进程回收前缀两项 ok。该次通过不反推旧失败或 N18-F01、N19-F01/F02/F03/F04、N20-F01 原因归属。N20-F02 现有 ready 后同步 receipt 读取无独立完整墙钟上界的残余边界仍适用，不能声称覆盖所有同步 I/O。

## 保全与释放

原根 `../nhc-d-default-composition/NoManCode/rust-app/.local/next20-d-c2efa28d-attempt1/` 留原 runner、各步命令/输出/状态、源码清单及测试保留现场；18188 项旧文件缺失或变化 0、读取错误 0，结束清单 19922 项。前后及各步 180 项源码与固定 blob 一致；最终 status/index/unmerged 空、规定重型进程为空。旧 b5300c25、e0c2f27b 及更早失败现场均保留；无泛杀。完整 UTC 精度和环境见原日志。

D 重型窗口已释放、独立树冻结；等待经理审证并安排 C。未合入 main，也未声明 NEXT-20 联合验收或整个项目完成。
