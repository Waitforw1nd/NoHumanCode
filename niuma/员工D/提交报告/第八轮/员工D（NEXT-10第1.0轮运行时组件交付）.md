# D-R8-01 第1.0轮运行时组件交付（含N10-D01）

2026-09-27，员工D GPT-6-astra，执行ID D-R8-01-20260927-E01。提交待经理review，非NEXT-10整体验收。独立树 ../nhc-d-plugin-control，分支codex/d/plugin-control；本人只提交授权两源码，主仓index/管理提交和所有跨树merge均经理。

## 固定版本与范围

完整基线fb6fe096a4ab3209002bfbb24c51872099530191；API小步3bda832de270d5928cbbada441e537e7ea7dcb2b；初次完整组件7d16de89491be8c21b8c382b97fc874d3dfcdd24；本次N10-D01修正候选 **0f384aa4da6777753c6c199bb80b4a4e5d6aaec7**。NoManCode tree **5ed1e4beffc45070041cfb006629d560c067c418**，Rust tree **a000c98c2046476022af5e260fbf65a091410282**。提交后及最终验证后status/index空，基线→候选仅src/tool_runtime.rs与tests/tool_runtime.rs。

文件SHA256：src/tool_runtime.rs=5B1C36481B3E6BABE7498082617248F44E3B6C49D82D0FE8956007352BCA4D3A；tests/tool_runtime.rs=C5A63D8C89CBC66A1047A964889E11B90D9BB876BF016EB06F7649A0A00E791E。

运行时API与[第0.1报告](员工D（NEXT-10第0.1轮可编译API候选）.md)完全相同，也已同步B/C。with_files_control同mutex提供短同步callback，FilesControl仅snapshot/reconcile。false构造跳过激活；无实例时状态明确provider/consumer各Unloaded。状态不公开Host/服务Arc/任意插件错误message；DTO均双向serde、稳定snake_case。

RAII文件计数仅在admit成功后增加；所有fallible任务快照/服务解析在admit之前。返回后执行、错误、未执行丢弃、执行中future取消均Drop减数，不重取runtime锁。compat不计入文件lease。新文件准入及definitions同时检查admission、当前实例及本次应用失败状态，禁用没有fallback。

启用只有provider+consumer全部Active才打开准入；失败保留failed、回收effect，Host的激活失败未自动写recovery由Runtime补安全cause。停止始终consumer→provider，consumer停止失败不阻provider清理；历史recovery保留。再次显式启用先停止残留、unload非Active、再建固定assembly，不直接start Failed；成功后当前Active不被历史recovery污染。

## N10-D01经理review修正

7d16de8的第一次停止失败真实显示Failed，但后续新的false命令也会一直Failed，不能实现同状态显式协调。经理指出后修为：进入新false时检测已有失败；本次stop无新失败才unload旧失败/已停实例、清本次apply_failed，历史recovery仍在。首次停止失败绝不在同次隐式重试；GET/旧key回放只snapshot不清理。测试证明第一次false→Failed、snapshot保持、第二次显式false→Unloaded、零额外deactivate且recovery原样；失败初始激活也可被显式false清理。修改范围仅本人模块，无API变更。

## 组件证据矩阵

| 场景 | 证据与真实边界 |
|---|---|
| F02 | snapshot只读一致、双向serde、provider真实tool.read_file/tool.write_file及owner、两个Active实例、effect=2/subscription=0；registry marker正控和真实读写沿旧Workspace执行。组件自身没有Store/Provider，DB/HTTP/List不变由B/C验证。 |
| F03局部 | stop先赢文件admit callback panic断言不触发、definitions只有compat；公开disabled构造拒read/write，允许原权限内四compat准入。真实审批/claim事务是B证据。 |
| F04 | claim先赢有界双向通道，disable等待同mutex，返回lease后effect已清但in_flight1/drainingtrue，旧lease可执行；真实write_file先lease后disable仍实际写once，后续文件拒绝；正常/路径错误/任务绑定错/claim失败/Drop持锁/pending abort全部计数归零；compat不计数。持久管理receipt及真实Engine claim/结果落库由B证明，D合成receipt不冒充DB事实。 |
| F07 | 测试专用factory同生产from_host路径注入provider/consumer激活错误、panic与双停止错误，失败查询可用/准入关/effect清/recovery安全；显式启用重建成功保留历史；旧false失败由新false显式清理。失败callback不改运行时仅证明锁内组织，真实SQLite事务失败及进程崩溃由B/C证明。锁毒化definitions/acquire/control全部拒绝。 |
| F10 | false构造两插件activate事件计数均0；后续两轮精确provider→consumer start、consumer→provider stop；重复启停不残留effects/subscriptions，无公开原始服务句柄，真实compat list_files仍可执行。 |

原有并发测试无界Barrier已改为5秒recv_timeout通道；新future通知/abort用5秒tokio timeout。不是靠睡眠幸运窗口。故障factory/Probe/Pending全部cfg(test)内，无生产环境开关。private from_host是相同生产构造路径提取，未改plugin_host/catalog/lib/Cargo/build。

## 实际验证

0f384aa固定源码，从2026-09-27T03:06:50.8314510Z到03:07:17.0920899Z，以下均退出0：

- 未改build.ps1 -Action check（组件编译检查）。
- cargo test --locked --lib tool_runtime::tests -- --nocapture：**14通过/0失败/0忽略**。
- cargo test --locked --test tool_runtime -- --nocapture：**3通过/0失败/0忽略**。
- cargo clippy --locked --lib --test tool_runtime -- -D warnings：限定模块依赖静态检查。
- rustfmt --edition 2024 --check src/tool_runtime.rs tests/tool_runtime.rs：两授权文件格式检查，UTC在review01-fmt.json；git diff --check及提交cached检查0。

运行时故障注入panic钩子的输出是预期捕获反例，日志中测试最终通过，不是未处理的测试失败。proc-macro-error2未来兼容提示在check保留。

历史证据不覆盖新候选：3bda832 check0；初次工作区运行时12/0；7d16de8固定13/0+3/0、check/clippy/fmt0均保留；N10-D01后在0f384aa重新验证上述结果。最早Python读UTF-8源码曾因系统GBK解码错误退出1、未改文件，随后显式utf-8读写成功；不是产品测试失败。

原始/发布日志与元数据17对见[evidence-manifest.json](evidence-manifest.json)。每对原字节SHA256与发布SHA256已逐项回读核对；发布仅将运行时解析出的独立树绝对根替为../nhc-d-plugin-control，实际改写清单逐文件在manifest，不改UTC/结果/源行号。源原件仍保留独立树.local。验证TMP/TEMP=.local/tmp、CARGO_TARGET_DIR=.local/target，工程target为旧fixture准备；全在活动盘本人树。DEV/TEST_DEBUG=0、INCREMENTAL=0仅进程设置；机器MSVC只读来源，未安装更新/迁移工具链；实际工具版本在review01-fmt.json。

未执行最终组合五门禁、wasm-check、全workspace test、live/release/push；NEXT-09未作为本轮重测。未执行任何共享index/跨树merge，没有重写旧失败证据。当前无D阻塞；源码与第1.0档案交review后冻结，下一第一操作仅接经理明确review修订或固定组合只读复核，最终持久/HTTP/CLI与组合验收仍B/C/经理责任。
