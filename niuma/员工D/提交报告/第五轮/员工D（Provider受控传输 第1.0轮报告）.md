# D-R5-01 Provider受控传输 第1.0轮交付报告

2026-09-27，员工D / GPT-6-astra；执行ID D-R5-01-20260927-E01。D实现与定向已完成，当前候选abfb64d10cb1c23cc38008e597cda7f8bbcb372d、Rust treebef0e276008ed1793a0c6824e439097b4a274ee9，status空。提交待经理review；不是NEXT-07整体通过。限定Clippy因B文件lint失败，经理已通知原B修复，未降lint/跨写。

## 基线、所有权与提交

开发基线c5430b166c6c35307f60b75ce5a8738d44f9c2e1，Rust tree b549de2e18d39c8e69e6607b9f430974895ee4b9；独立树../nhc-d-provider-transport，分支codex/d/provider-transport。本人仅操作本树授权文件/index/提交，所有B/C依赖合入由经理串行执行。人员档案仅共享niuma/员工D，旧树/备份不写。

D提交：77d08e6真实transport API；43fc6ee Provider绑定及unit反例；3ac33d9环境代理同目标正对照；5063e41公开Engine集成；abfb64d SSE精确上限及超限未EOF。实际依赖组合bce914a后首次运行；B200964修复经经理合入28a0a9a后原样Engine集成通过，再固定abfb64d最终定向。累计本人源码只有src/provider_transport.rs、src/provider.rs、tests/provider_network.rs；lib/Engine/Store/schema/HTTP/CLI不属于本人实现。

## 实现与验收映射

| 范围 | 实现和实际证据 | 结论边界 |
| --- | --- | --- |
| 固定目标 | validate_target原输入检查control/空白/backslash/query/fragment/userinfo、dot与encoded dot、编码分隔符/百分号合法性；规范scheme/host/effective port和base path；unit规范化与拒绝表 | 默认端口/IPv6、base path保留已验证，不做字符串prefix授权 |
| 一次permit | 私有字段、无Clone/Debug；model/account user/header预校验；Provider调用前匹配第二对象；URL/method/key/model/header取permit，send再核JSON model | 生产构造属B持久claim后的准入；D不提供公开测试绕过 |
| redirect/proxy/retry | no_proxy、redirect none、reqwest retry::never；五状态301/302/303/307/308双listener B零连接/A仅一次POST；同target可达正对照；独立子进程同loopback环境proxy自动client正对照及explicit proxy正对照 | Windows变量名大小写不区分，不把命名分支当独立Windows语义；代理数据均fixture，未联网外部 |
| JSON读取 | 精确4,000,000字节成功；超限响应尚未EOF即Unknown；完整HTTP失败/完整坏JSON/截断分型 | 不先完整text再检查，不回显上游错误正文/Location |
| SSE读取 | finish_reason+完整DONE，拒绝截断、无finish、无完整结束、坏事件和emit失败；精确4,000,000成功，超限未EOF即Unknown | 正常DONE不必等待长连接关闭；超限/截断不自动重发 |
| 真实Host授权消费 | 公开Engine configure/preview/confirm/check/models/account；有效route/key缺grant和check用途不全时TCP/HTTP均0；重确认后同端点真实成功；两call回执查询一致 | D覆盖管理路径，不冒称全部Task/start/resume竞态/真进程kill，后者归B/C |
| 目标/凭据变更 | A→B端口/base path、model变化、raw put_secret值变化拒绝；重授权只携正确key；账户保存零网络、route授权不覆盖balance、user/token变化再确认 | 全程DPAPI持久key，不换env绕过授权失败 |
| durable结果 | redirect为finished失败，截断为unknown，同ID可查；关闭Engine/Store后重开查询不重发，B目标有可达正对照 | 这是同进程重开验证，不冒充真进程崩溃；真kill证据归B/C |

静态TransportError四类无source/原始reqwest错误；Invalid仅发送前确定失败，Response为完整非2xx，InvalidResponse为完整无效业务，Unknown为发送后无法确定完整结果。不会把已发失败记not_sent。返回原业务成功形状，call存储/HTTP映射由B/C负责。

## 实际命令与结果

在本人E盘NoManCode/rust-app执行，正确VS/MSVC/SDK初始化，进程debug0/incremental0；未改工程build配置。最终候选abfb64d固定后：

| 命令 | 退出码/结果 | 日志 |
| --- | --- | --- |
| cargo test --locked --lib provider -- --nocapture | 0；17通过/0失败 | d-r5-provider-02.log |
| cargo test --locked --test provider_network -- --nocapture | 0；4通过/0失败 | d-r5-network-03.log |
| cargo clippy --locked --lib --test provider_network -- -D warnings | 101；store.rs:968-978 collapsible_if（B所有） | d-r5-clippy-01.log |
| rustup run stable rustfmt --edition 2024 --check（三个D文件） | 0 | d-r5-fmt-01.log |
| git diff --check | 0 | d-r5-diff-check-02.log |

17顶层测试含一个proxy child辅助入口，父用例实际启动三次子进程；不作为17个独立业务能力。最终测试合计21通过0失败仅限定上述两个筛选，不是全workspace门禁。生产check已在bce914a退出0（d-r5-check-01），不冒称最终SHA另跑check；最终两测试已实际编译其依赖。最终五标准门禁唯一C，本人未运行release/live/push。

历史失败完整保留：bce首新Engine集成d-r5-network-01为0/4、exit101，真实发现N07-B04 credential_ref误触脱敏；经理派B修，28a0a9a原样复跑d-r5-network-02为4/0。首失败测试提交前仅将tempdir目录明确到target，未降低断言；当前夹具/target/日志均E盘。首16/0低层仅绑定bce，当前17/0另有最终日志。

原始日志在本人树.local/review-evidence，发布日志只将绝对本人树前缀替换为<D_WORKTREE>，保留事实；原始/发布SHA256、候选及三源码hash见[manifest](证据/manifest-1.0.json)。第0.1/0.2/0.3报告不重写。

## 待完成与风险

经理安排B修store lint并串行合回后，D可补限定Clippy结果，不自行改B。当前不申报全组合通过；Task授权快照/撤销竞争、schema/恢复/命令ack、HTTP/CLI/web属于B/C交付及组合验收。没有OS网络沙箱/任意命令离线承诺，没有新增网络工具。本轮无D源码之外schema更改、无新依赖、无真实Provider请求。

下一第一操作：保持源码干净待经理review/最小B依赖；补验证后更新本人身份与新轮次报告，交固定候选供C串行最终组合。
