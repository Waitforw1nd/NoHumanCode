# 员工D NEXT-12第1.0轮Shell组件提交

2026-09-27，员工D GPT-6-astra；执行ID D-R10-01-20260927-E01。实现基线6c84681f6ffc47afb521c472a181f6f0217f4fe3，独立树../nhc-d-shell-service、codex/d/shell-service。本人只提交授权五文件组，经理独占跨树merge与主仓index。

固定候选bd6b8876c936382a64f5777862bd644ff3dde3fa；NoManCode treec88e8163a4a990c6b4ebc2f2d189ac8df9a00c65，Rust tree2c90599921c41fb81437a34571a660a094534e85。先行生产API48797ca1efe609857c7ba7caa7c444492a45077d已交经理合B；新提交增加组件、ReaderTask abort-on-drop防detach、lib声明排序，以及经理授权的cfg(test) once hook。当前源码/index空，无构建运行，已交可整合窗口。

## 已实现与证据

独立shell provider/consumer真实registry，files1.2/shell1.0，两独立Host/Mutex/desired应用状态/recovery/Atomic计数；任何路径不同时持两锁。run_command移出compat，无隐藏直造回退。双bool构造与shell控制API按冻结契约；files算法无改动。shell lease worker拥有Task/args/service/count，caller丢弃只停止等结果，原准入可在停用后完成。command_execution统一绝对deadline覆盖直接child和两个reader，固定5秒总cleanup；ReaderTask drop abort只是异常兜底，正常和受控失败均显式join。移除旧taskkill，范围明确为直接child等待与输出收尾，不承诺OS后代全灭、Host强杀后清理或Job Object。

最终固定组件与检查如下，均exit0：

| 命令/范围 | 结果 |
| --- | --- |
| 原build.ps1 -Action check | workspace check通过 |
| cargo test --locked --lib tool_runtime::tests -- --test-threads=1 | 26通过，0失败/忽略 |
| cargo test --locked --lib command_execution::tests -- --test-threads=1 | 4通过，0失败/忽略 |
| cargo test --locked --test tool_runtime | 4通过，0失败/忽略 |
| cargo clippy --locked --lib --test tool_runtime -- -D warnings | 限定范围通过 |
| rustfmt --edition 2024 --check --config skip_children=true，授权五文件 | 通过，原始日志真实0字节 |

合计34组件通过（26+4+4），不是全量库测试，更不是最终五门禁。先行25+3、后续9+4属于过程验证，不累加当最终数量。所有UTC/实际命令/退出码与SHA在元数据，原始/发布副本与hash见[证据清单](D-R10-01证据清单.json)。最终check在提交前对同一源码运行，随后提交固定上述tree；组件/Clippy/fmt按bd6b8876运行。过程日志全部保留，无本轮失败测试被隐藏。环境target/TMP/TEMP/log只新E树.local，debug0/incremental0/jobs2；Rust/MSVC既有安装只读，未改Cargo/build，proc-macro-error2未来兼容提示保留。

S01/S02：真实command stdout/stderr、exit7与文件marker；files停用/卸载/缺注册Failed时shell仍可执行，shell停用/卸载/缺注册/停用失败时四files真实读写列搜均可用。元数据四effects与一effect、各两实例、权限声明process.exec、独立recovery/count。两组false组合无激活，all false仅wasm；N10-D01两组首次失败保留、显式false清旧Failed。

S05：实际命令写ready后abort外层caller，worker count仍1、files为0，停用shell呈draining且effects0；显式release后真实finished marker且count归0。未poll lease drop、绑定变更拒绝、参数失败、计数overflow拒绝admit、已准入停用后真实执行均检查。worker受控panic证明通道消失返回typedunknown，不把该注入冒称真实OS失败。

S06：真实直接child超时后确认PID退出、reader guards归零，返回确定timeout而非假unknown。真实后代继承pipe、直接parent先退出，2秒统一deadline及时结束reader等待；测试随后明确kill+wait该已知后代并核两PID退出。产品不负责保证所有后代退出。reader/wait异常和cleanup未确认是私有cfg(test)故障注入，证明typedunknown分类/收尾，不声称实测OS拒绝杀进程。

经理批准的跨所有者测试接口位于私有command_execution：pub(crate) unknown_after_spawn_once(task_id:&str)->UnknownAfterSpawnGuard，仅cfg(test)。按Task一次消费，guard保留至Drop，重复同ID拒绝不覆盖不poison；真实spawn后受控失败、经真实child和reader收尾返回CommandOutcomeUnknown。异Task正控、消费后正常命令、未消费Drop、真实PID退出均有组件证据。B仅engine模块消费此hook证明claim/unknown传播，不扩lib。

测试自身PID查询/后代helper的成功分支实际wait已完成；未触发的watchdog失败分支只以kill_on_drop兜底，不能宣称该分支也已证kill+wait。所有轮询都有deadline；原始日志/旧树/产物保留。

## 未完成与下一步

本报告交S01/S02/S05/S06组件，不代替B审批/持久/Engine链或C真Host/HTTP/CLI/最终五门禁。经理新增S08联合失败证据已接收，待经理把B固定API及Engine::new_with_runtime_for_test合入本树；D随后只在本人tool_runtime模块补真实故障shell经Store→Engine set_plugin_enabled的accepted receipt/实际Failed证明。当前源冻结待经理整合，下一第一操作核合入SHA/tree，再执行限定S08；不自行merge、不跨写B文件。