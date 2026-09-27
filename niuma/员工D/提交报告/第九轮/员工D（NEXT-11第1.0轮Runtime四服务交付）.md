# D-R9-01 NEXT-11 Runtime四服务第1.0轮交付

2026-09-27，员工D GPT-6-astra，执行ID D-R9-01-20260927-E01。经理明确正式新实现派发；PRE01只读报告保持原事实，不计实现证据。状态：组件提交待审，非NEXT-11整体验收。独立树../nhc-d-file-discovery、分支codex/d/file-discovery；本人仅提交两授权源码，跨树merge/共享index/管理归档均经理。

## 固定候选与API

完整基线11b9bee8c05a4c05396ae18206a693e15bebc26a。可编译API检查点1ea0d49f8354a81bf9f680688ec6b91b7376ebc8；首固定组件053e5150100c6637c3792ac81baff8efa0bb645b；最终候选 **9a10c7246fd8cb4e73af9e86fb828e4012f01d8a**。NoManCode tree **aa7bc80fbcf1e628b9553b6eb81fe693f040e0e8**，Rust tree **730935b55eb26f024e2076d75c01545ea66b092e**。最终status/index空，基线到HEAD仅src/tool_runtime.rs、tests/tool_runtime.rs。

文件SHA256：src/tool_runtime.rs=8D09448966DDA6419863425D0248E6AFE2E0A88EB68D661EB0FA65D6654E99A9；tests/tool_runtime.rs=1C2EDA4714C388E3493E00229F4B4BF6C01E82B06E00AEB37D616FD2A183170C。

按契约提供pub(crate) FILES_PLUGIN_VERSION:&str="1.1.0"、FILE_TOOLS:[&str;4]=[read_file,write_file,list_files,search_files]、COMPAT_TOOLS:[&str;2]=[run_command,run_wasm]。provider与consumer manifest均1.1.0（manifest_version仍1）；四service接口各1.0.0。生产与测试私有服务数组均四项，list/search使用真实绑定WorkspaceService，移除compat直分发；原control/DTO/acquire签名不变。常量和manifest numeric version有debug一致性断言。

Host/catalog/Workspace/Engine/Store/lib/Cargo/build未修改。双实例仍2，健康effects4/subscriptions0，实际provider keys按Host稳定排序list/read/search/write。新增读服务自动沿FILE_TOOLS走准入关闭、单次lease及RAII计数；持久Store三读复核/状态DTO组合由B真实依赖接入，不在本人树造stub。

## L01–04组件覆盖

| 场景 | 实际测试证据 |
|---|---|
| L01四registry来源 | 四个独立marker服务在不存在的workspace执行成功，四key definition含marker、各执行计数1；同时核双manifest1.1.0、四接口1.0.0、无重复definitions、provider四key/owner、effect4、双Active实例。不是只有名称列表。 |
| L01真实FS与权限 | 真实目录两文件list结果及search第2行marker精确匹配，并与原Workspace结果对照；write_scopes为空仍可只读，tools=false两发现工具拒绝且admit不触发；两工具分别越界与.git禁路径执行错误，计数归零。算法原样复用，未声称修复所有文件系统TOCTOU。 |
| L02禁用/卸载 | 两种状态definitions只保留获权compat，四项acquire均拒绝且callback panic不触发，list/search marker计数不增，无fallback。公开disabled构造同样四项拒绝、compat仅command/wasm。生命周期failed缺注册时四项全部拒绝。 |
| L03在途 | list/search分别用5秒ready/release/完成通道固定admit先赢、stop等同锁、发lease后stop完成effect0且in_flight1/draining；禁用后新准入拒绝，旧lease仍完成真实list/search结果并归零。线程结束is_finished带5秒deadline再join；非sleep幸运窗口。 |
| L03释放 | 混合read/write/list/search四lease计数4，禁用后4、锁内drop两项→2、search执行错误→1、list绑定拒绝→0；两发现工具各claim callback失败、锁内未执行drop、路径错误均0；两工具各注入pending service，真实执行future abort后有界等待取消完成，计数0/准入仍关。注入测试不冒称同步FS可被中途取消。 |
| L04生命周期 | 沿既有false构造计数零、精确provider→consumer启动/反向停止、循环4→0→4、激活/停止错误和panic、query不修复、保留history显式恢复及N10-D01测试。新增provider注册三项但缺search，Host ProvidesNotCovered→Failed/全部effect清/四项拒绝，新false显式unload再enable恢复4且旧recovery保持。 |
| L04兼容正控 | 原disabled list_files正控改为明确有权限run_command执行安全Write-Output，核exit0/stdout marker且in_flight0。此处admit callback只是组件正控，不冒充真实审批与网络ack；B负责Engine该链，run_wasm Engine拒绝保持由其与最终组合验证。 |

旧read/write真实文件结果、单次write禁用后完成、绑定不可改、锁毒化fail-closed、claim失败计数、历史恢复等本人组件断言保留并适配四effect。没有删除有效负例来制造通过。

## 固定验证、失败与证据

最终9a10c724序列UTC 2026-09-27T03:54:28.7509495Z→03:54:32.5982568Z：未改build.ps1 -Action check退出0；cargo test --locked --lib tool_runtime::tests -- --nocapture为 **20通过/0失败/0忽略**；cargo test --locked --test tool_runtime -- --nocapture为 **3通过/0失败/0忽略**；cargo clippy --locked --lib --test tool_runtime -- -D warnings退出0；两授权文件rustfmt --edition 2024 --check退出0（独立UTC见final-fmt.json）。git diff --check及提交cached检查0，最终status/index空。

保留中间事实：API 1ea0d49 check0；未提交工作区20/0；053e515固定check0、20/0+3/0、fmt0，但限定Clippy **101**，tests/tool_runtime.rs:79单元素for循环被single_element_loop拒绝。9a10c724只将其改为等价单次同断言，未放宽测试/改生产，随后重新运行上述固定序列全0。旧失败日志和元数据未覆盖。故障注入panic为被测试捕获的预期输出，非测试失败；proc-macro-error2未来兼容提示保留。

17份日志/元数据原始与发布双hash见[D-R9-01证据清单](D-R9-01证据清单.json)，已回读发布字节核hash。原件仅本人独立树.local按原字节保留；主仓只写发布日志及manifest，不再复制原件。发布仅把运行时解析的独立树绝对根替为../nhc-d-file-discovery，每文件path_replacements明确记录，UTC/退出/测试输出不改写。全部TMP/TEMP=.local/tmp、target=.local/target及日志在活动新树；工程target兼容目录已准备。DEV/TEST_DEBUG=0、INCREMENTAL=0仅验证进程；现有MSVC只读，不安装更新。

未运行全workspace test/最终五门禁/wasm-check/live/release/push；未改共享index或跨树merge。NEXT-10旧通过不计本轮新结果。B/C尚须三读Store/Engine最终准入、持久旧receipt/Task保持、真Host kill+wait/CLI双版本等联合证据，D组件不冒称这些已完成。

无当前D阻塞。源码及本报告/身份交审后冻结；下一第一操作仅接经理明确review或固定组合只读任务。所有旧树/产物/失败证据保留。
