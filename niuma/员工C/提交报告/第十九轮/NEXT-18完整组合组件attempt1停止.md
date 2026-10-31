# NEXT-18 C 完整组合公开组件 attempt1 停止

2026-10-31，员工 C / 6.1 Terra，执行 ID `C-R19-01-20261031-E01`。经理唯一组件窗口固定 HEAD `283a29858b914c1f26318f3a9603739ebc46be1d`、NoManCode `88d9baeda5df6996b3b3b48957aaea60bcd523c7`、Rust `12451bb973c48cf2bbbeade0c89fd5a776c905e6`。原始日志在 C 树 `.local/logs/next18-c-component-attempt1/`，target 为该树 `.local/target/`；驱动 `.local/next18-c-component-attempt1.ps1`。环境 offline/locked、debug0、incremental0、jobs2，既有工具链只读。所有 UTC、命令、退出码、源码清单及环境见[发布证据 manifest](NEXT-18证据/组件attempt1/manifest.json)及 meta。原件与发布文件 14 对 hash/长度已逐对核验，6 份仅路径转换；manifest SHA256 `1C368B2BD656BA0F6ACCC6D7BD3B7E7259D8C37254F7E364378A859D58248185`。

| 顺序 | 实际命令结果 |
| --- | --- |
| check --all-targets | exit 0 |
| build --bin peachsh | exit 0；CLI 29,572,608 字节，SHA256 `477DC4A7EC90B2B533C0494D97ACBD1C52FD909C894361F35B590D134DA1F4F6` |
| lib runtime_management_http_tests | exit 0；实际选中 1，1 passed / 0 failed / 0 ignored，313 filtered |
| 两个 integration target | exit 101；CLI 6/0/0；workflow 6/1/0；失败 `draining_busy_does_not_consume_key_and_old_receipt_replays` |

session 68609 已自然退出 1；本窗口整体未通过，不得累加为最终结果。八份前后源码清单各 168 跟踪文件且 SHA256 均 `F445B34C5B96A6EDA8F86E330B7C40357A72FE190712718EE837644D9F21EB51`。2026-10-31T07:01:45Z 本人实核 HEAD/双 tree、分支均保持，status/index/unmerged 空、命令行关联本人树进程无匹配，已停止构建并释放窗口。

## 失败现场与只读定位

失败任务输出 `working`，终态 failed、错误“网络调用结果未知”，`done()` 原要求 completed；失败前已执行并通过 draining、Busy 和原 disable receipt 回放断言，但后续收敛/启用断言未到达。fixture hold 分支首帧 `finish_reason=null`，release 后直接发送 `[DONE]`；`provider.rs` 要求 `[DONE]` 前有 finish reason，`provider_runtime.rs` 将 decoder 错误映射 Unknown。此静态路径与输出吻合；该次无逐帧抓包，不能把其他可能性排除或仅据此断言生产逻辑通过。既有 Provider 总超时 180 秒，现场没有超时来源证明。经理已确认限定夹具修订：release 后先发送合法 stop 完成帧再 `[DONE]`，不改生产解码或降低断言/timeout/并发，修后须重新实际验证。

Host 子进程 stdout/stderr 配为 null，无原始 Host 日志可恢复；失败任务对应的临时根 `.local/tmp/.tmp27CuwH` 已不存在，未能核其调用账本或事后补取现场。`.local/tmp/.tmp90yMXt/runtime.db` 唯一残留数据库原件保留并逐字节复制到本次原始日志目录 `unattributed-runtime.db`，450560 字节、SHA256 `6246F61E227944961197790963637A71E8E1CA62E9572FB9A8ABA6F4EB3B6AAD`；未确证归属，不将其冒称失败任务现场。

两个人工截断测试虽各自输出 ok，但 Hyper 因声明 Content-Length=100、固定 body 长度1触发服务任务 panic。原日志能证明客户端 unknown 与各测试通过，真实Host代理测试的既有断言还证明提交及原键回放；不能证明已送出部分 HTTP 响应或将该 panic 冒称正常截断。经理另授权两指定测试文件改用可观测真实响应头、部分字节和关闭的受控截断；原失败与日志保留，修后另报。

最终五门禁未放行、未执行，完整项目和 NEXT-18 未验收。下一步仅按经理 C18-R04/R05 在两测试文件静态修订，完成后冻结交审，未经新放行不构建/复测。
