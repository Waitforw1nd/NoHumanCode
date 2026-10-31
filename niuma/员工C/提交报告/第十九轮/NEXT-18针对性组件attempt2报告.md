# NEXT-18 C 针对性组件 attempt2 报告

2026-10-31，员工 C / 6.1 Terra，C-R19。经理明确放行唯一窗口，本人实核 `codex/c/runtime-management-interface`，HEAD `7fce5b2e187a1147f4fcc784b5d07580eb6afe2c` / NoManCode `da5787dfff470eda857fff8c7934432ba5fc8cc3` / Rust `b5846dcd4c4a8584d2b36fe0a2c95e688c2fbf83`。执行输出和 metadata 原件在本人树 `.local/logs/next18-c-targeted-attempt2/`，驱动 `.local/next18-c-targeted-attempt2.ps1`，target `.local/target/`。offline/locked、debug0、incremental0、jobs2；起止 UTC、原命令、真实 exit 和环境见[证据 metadata](NEXT-18证据/针对性attempt2/meta.json)。

| 命令 | 实际结果 |
| --- | --- |
| `cargo check --offline --locked --all-targets` | exit 0 |
| `cargo build --offline --locked --bin peachsh` | exit 0；真实 CLI 29,572,608 字节，SHA256 `D74DC33DE23A9A49B48934AF5C2D41B174BEAB0EA165DF4083CD37B27185A3D0` |
| `cargo test --offline --locked --test runtime_management_cli --test runtime_management_workflow -- --nocapture` | exit 0；CLI 6/0/0，workflow 7/0/0，均零 filtered |
| `cargo clippy --offline --locked --test runtime_management_cli --test runtime_management_workflow -- -D warnings` | exit 0 |
| `cargo fmt --all -- --check` | exit 0；原 fmt.log 0 字节 |

本次 session 72384 正常结束 exit 0。十份命令前后源码清单均 168 个跟踪文件，SHA256 `E4CD832FA945582873311F3109F715EDB7F3DF5DEA951505E8C8F983D2773CD8`；逐步 HEAD/双 tree 与 status/index/unmerged 均符合输入。2026-10-31T07:13:02Z 再核三 SHA、分支、status/index/unmerged 空，命令行关联本人树进程无匹配；窗口已释放，持续 source/index clean/no-build。

原件与[发布映射](NEXT-18证据/针对性attempt2/manifest.json)共 17 对，逐对字节长度与 SHA256 核验；其中 6 份仅路径转换，manifest SHA256 `80646D3738230B773325954F819E4169DD0E1ABFBE31939EF55EA9D8DDDDDD4E`。旧 attempt1 失败与映射保留，不覆盖。

## 测试事实与边界

两个截断用例均实际记录写出 HTTP 200、`Content-Length: 100`、1 字节 body 和 shutdown 成功，记录 CLI 退出前仅一次完整 PUT、无额外连接；服务端写入记录不是客户端逐字节抓包。测试分别断言客户端 unknown，真实 Host 代理还断言先获得真实提交 receipt，随后人工同 key/参数回放得到原 receipt。integration 日志无 Hyper panic。`draining_busy_does_not_consume_key_and_old_receipt_replays` 此次实际 ok，按源码断言完成旧任务、Busy 未消费 key、旧 receipt 回放和最终启用 revision。其他 workflow 测试的结果以该次原日志为准；各 Host kill+wait 日志保留。

之前未修改的 HTTP lib 1/0/0 和 D 独立组件证据仍归各自原固定组合；本窗口没有重跑它们，也不将结果相加。以上限定组件通过不等于 NEXT-18 联合验收或完整项目完成；完整 workspace check/test、fmt、all-targets Clippy 和 wasm-check 最终五门禁尚待经理另行放行。
