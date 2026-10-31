# NEXT-18 C 公开组件 attempt1 审查

2026-10-31，项目经理。结论：CHANGES_REQUIRED，原C-R19接续。主线仍NEXT-17，不进入最终五门禁或联合验收。

固定候选 `283a29858b914c1f26318f3a9603739ebc46be1d`；NoManCode `88d9baeda5df6996b3b3b48957aaea60bcd523c7`；Rust `12451bb973c48cf2bbbeade0c89fd5a776c905e6`。经理已读原始integration.log、lib.log及meta.json；all-targets check/build exit0，HTTP实际选中1项并通过，CLI6/0/0、workflow6/1/0，integration exit101。

原始日志在 `../nhc-c-runtime-management-interface/.local/logs/next18-c-component-attempt1/`。C本人回报三SHA/status/index/unmerged空、无关联进程，已释放窗口并冻结；后续只授权静态夹具修订。原件发布映射和经理完整hash核验另记，不提前认作核验完成。

## C18-R04：排空夹具缺少合法终帧

唯一失败为 `draining_busy_does_not_consume_key_and_old_receipt_replays`。旧任务输出已有working，但终态failed、错误为“网络调用结果未知”，不满足原completed断言。

经理核对 `tests/runtime_management_workflow.rs:59` 起的hold路径：先发finish_reason=null的内容帧，release后直接发[DONE]。`src/provider.rs:119` 要求[DONE]之前已见完成原因；`src/provider_runtime.rs:585` 起将decoder失败映射为Unknown。因此该替身不能作为正常完成的在途请求。源码与实际失败输出吻合，但本轮没有逐帧抓包或可归属的失败数据库，不声称排除了所有环境可能性；必须用修正后动态结果闭合。

授权仅在hold路径release之后先发合法finish_reason=stop终帧，再发[DONE]。原进入/释放屏障、任务completed、draining/Busy不消费key、原receipt回放及最终revision2保持；不改生产Decoder、timeout或并发，不把failed改成可接受结果。关联原C18-R02公开排空动态证明仍待通过。

## C18-R05：框架panic没有证明实际部分响应

两个用例分别为CLI `truncated_write_is_unknown_and_sent_once`、真实Host代理 `committed_put_lost_response_has_one_send_and_explicit_real_replay`。固定Body仅1字节但声明Content-Length100，引发hyper连接任务panic；测试结果ok证明客户端报unknown，以及代理已保存提交receipt并能显式回放，不能证明部分HTTP响应已发出。保留日志中的panic，不把它写成真实字节截断通过。

在原两测试文件内改成可控连接截断夹具，记录并断言真实HTTP响应头、不足声明长度的body写出和连接关闭、单次请求。真实Host代理仍必须先转发完整请求、确证提交并保存原receipt，然后截断返回；保留unknown、无自动重试及同key同参显式回放断言。可采用test-only原始TCP辅助，不新增依赖或生产入口。若采用流式Body，必须证明实际部分发送与关闭且没有框架panic，不能只靠测试名或handler调用计数。

## 修订与证据边界

唯一写入人/提交人仍C，仅 `NoManCode/rust-app/tests/runtime_management_cli.rs` 与 `NoManCode/rust-app/tests/runtime_management_workflow.rs`；经理负责审查与跨树整合。基线为上述283a298，Git仍原独立分支/工作树。

先封存attempt1报告和原/发布映射，再做上述两文件静态修订、限定格式/diff检查，提交固定HEAD/双tree和clean报告。没有重开构建或测试窗口；经理审后仅重验受影响integration及必要静态检查，最终固定完整组合五门禁不取消。

本轮Host stdout/stderr原设为null，失败临时目录已不存在；残留runtime.db不能确认归属，不冒充失败现场。禁止补造现场或把新执行日志归给旧失败。经理没有修改产品或运行产品测试。

## 原件核验追加

经理已独立核完14对原/发布文本的长度和hash，6份仅路径前缀转换；8份源码清单一致，168项与受测Git检出内容一致，其中164项逐字节匹配blob，4个cmd按仓库.gitattributes的eol=crlf转换匹配。四步起止身份及clean、实际测试数和退出码一致。manifest SHA256 `1C368B2BD656BA0F6ACCC6D7BD3B7E7259D8C37254F7E364378A859D58248185`。见[经理核验](../../../项目经理/审查记录/2026-10-31NEXT-18-C组件attempt1原件核验.json)。

未归属数据库原件与独立副本450560字节/hash一致；仅核字节保全，没有将其SQLite内容用于任何失败归因或产品验收结论。以上核验不改变CHANGES_REQUIRED或两个夹具问题尚待修后重验的状态。

## 7cf365 静态候选增量

经理完整读 `7cf365338c14cfb21a9d329151279723e29cb5b2` 相对283a298的两文件差异。R04在释放之后补stop帧，保持原完成/排空断言，静态可进入验证。R05已改成完整请求读取、真实Host提交回执后写200/JSON/Content-Length100的响应头及1字节body并shutdown；但500ms连接监测结束后listener被释放，CLI可能还没退出，迟来的重试无法观察。该候选不放行动态窗口。

沿用R05授权原C静态补强：截断后持续监听额外连接，直到父测试确认CLI进程退出后发出完成信号；任一额外连接失败，不靠扩大500ms。保留既有CLI外层timeout/并发。CLI raw分支另补原通用handler已有的x-peachsh-token精确校验，避免重写夹具时丢失断言。仍仅两文件，无生产修改，新候选冻结后再审。
