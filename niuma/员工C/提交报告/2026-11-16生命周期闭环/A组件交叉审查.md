# C 对 A 组件的独立静态审查

日期：2026-11-16。审查 `82db0ca1e5de3e174d675857bc2d8b747266bbd9`，并继续核对 `aa0e94a4c946e5c47d0671361ec77c5422cfcfa7` 的原 holder 线程句柄移交、`e1be5de501efebad16fc45310b866a6604e834c3` 的成功 EOF/join 观察字段。未执行 Cargo、组件探针或真实进程。

范围内未发现可静态证明的新阻断；仍为待动态验证，不是组件验收。

- wait_with_cancel 将原 wait pin 住；token 分支只发停止请求并继续等待同一 holder，未 Drop/重启 collector。较早取消的回收界由共享 cancel_at 和 watchdog 处理，普通已证取消仍可 Settled。
- reader 成功只有实际 EOF；失败 join 之后代表无该 reader 的在途 I/O，但先 seal 并保持 Unknown。结束判断共同要求原 process signal、同一 Job active=0、两个 reader join；父退出或双 EOF 不能单独释放。
- final-read overflow 修复在完成 join 的同步点后再读 overflow，防止最后一次读取在循环早期检查之后越界仍被采纳为 Exited。C 独立测试让父先退出、reader 后读完再放行 join，保留 OutputLimit 断言。
- seal_unknown 在 LeaseCell 同锁中先封门再发布；settle 回调锁外消费。槽覆盖回调与 watchdog 真实退出，并在保留的原线程对象 signal 后归还。aa0e94a4 用线程创建返回的 JoinHandle 转 OwnedHandle，通过 channel 移交后才取 ProcessSpec 开始目标 OS 工作；线程创建失败有未启动退票，移交失败保留原线程对象直到信号，消除了旧自 DuplicateHandle 失败永久漏槽路径。
- 新 breakaway control 把 helper 信号、exit-code 查询、同一 Job 五秒内逐次 accounting、可能 breakaway child 的 receipt 证明分开；未知仍保持 holder。该修改不能推翻历史 case16 unknown。C 没有运行新 control，也没有操作历史事故对象。
- 新 EOF hooks 只在成功捕获且实际 join 后设置，普通 joined 字段与失败捕获区分；没有新增生产执行入口。

独立补证在 `NoManCode/rust-app/tests/support/lifecycle/component.rs`；正式 Shell/Host 路径在 `tests/execution_authority_lifecycle.rs`。真实 Store 能力由 B 提供，组件测试的仿射计数 lease 不得当作完整 U 验收。
