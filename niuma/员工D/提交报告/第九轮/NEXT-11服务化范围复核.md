# D-R9-PRE01 NEXT-11 list/search服务化范围复核

2026-09-27，员工D GPT-6-astra，执行ID D-R9-PRE01-20260927-E01。经理新派发的有界只读任务，不恢复已结项D-R8，不构成实现授权。固定输入主仓12cdf4c0e40125f38175959bd9917e8c68dfbd77，NoManCode9dabad1df5bf28251d390536deee43deb885e64e，Rust67a0f508d4e1aa69bb82c19eed84b2c3e2705e3e；开始实核HEAD/tree全匹配、status空。所有shell显式活动主仓cwd，仅git show/grep固定对象与本人档案写入；未改产品、测试、共享index或merge。

## 结论与建议边界

将list_files/search_files加入现有builtin.files provider及builtin.engine-tools consumer技术上可沿用现有Host/catalog和ToolServiceV1，没有确认必须扩改Host/catalog的阻断。建议此次仅改变服务装配与管理范围，不重写Workspace遍历算法，不纳run_command/run_wasm，不声称禁用撤销所有文件访问。下列遗漏反例须契约明确封住，不能仅改状态列表即宣布服务化。

## API和版本

- 保持builtin/builtin_with_enabled、with_files_control、FilesControl::snapshot/reconcile及现有状态DTO签名；provider/consumer仍两个实例，Host scope/固定持久plugin_id不变。FILE_TOOLS建议稳定顺序read_file/write_file/list_files/search_files；COMPAT仅run_command/run_wasm。
- FilesPlugin.services、assemble/assemble_enabled及构造和注入测试数组从2到4；四个WorkspaceService各按名称独立定义/执行。禁止raw Service或Host公开，不能让Engine直接走workspace::execute绕过registry。
- 新接口键tool.list_files、tool.search_files各@1.0.0；原read/write接口key@1.0.0不变，因为调用参数/返回及ToolServiceV1接口未改变。plugin_catalog说明插件版本与接口版本独立。
- 建议provider/consumer固定manifest版本统一1.1.0，以表达能力面新增；不是必须的技术前提，版本选择由经理冻结。若升级须同步PluginStatus.version及CLI固定校验/fixtures；若保持1.0.0须显式说明固定builtin装配能力扩展而非外部版本兼容承诺，不能一处改一处不改。仅运行时装配扩展不需要新DB schema：desired/receipt无服务版本字段，旧enabled/revision事实保持。
- 四服务正常effects_count=4、subscriptions=0、providers按Host key排序（list/read/search/write），双instance不增。affected_tools/compat_tools、human说明、稳定JSON及测试应一起改，避免CLI拒合法响应或仍宣称list/search不受禁用。

## 生命周期、lease与权限

现有manifest.provides及consumer.requires由FILE_TOOLS共同生成；Host检查全部provides覆盖，consumer检查全部bound payload类型，保持provider先启/consumer后启及反向停止。四service共享一组准入、持久期望和故障状态，不产生四个独立插件或独立revision。任何激活失败应整体关闭准入并撤销四effect；测试应注入部分注册后失败及consumer失败，保留安全recovery与N10-D01显式修复语义。false构造两个实例Unloaded且零factory激活，重复启停无残留。

in_flight扩大为四类已admit文件lease总数，原私有AtomicU64/RAII可直接复用。list/search现在也是旧lease可在disable后完成，而新acquire在admit前拒绝；这不保证取消旧遍历，也不将只读lease当持久approval claim。成功/执行错误/任务绑定拒绝/未执行drop/future取消均释放计数，Drop不取runtime锁，禁用不等待遍历完成。真实Workspace list/search主体同步FS且没有中途await；不得用单线程sleep制造“执行中可取消”承诺，应以有界预执行停点或测试服务future证明lease语义，真实结果另作正控。

Task.tools=false仍拒所有工具；list/search是只读，不能新增write_scopes要求（原本write_scopes为空可读），write仍受写scope，commands仍需allow_commands和既有审批。Workspace::execute入口verify_directory_policy、list子路径resolve、search递归目录policy/reparse/文件resolve与UTF8/大小/禁目录过滤均保留。search现有query1..4096、max_results默认50/clamp1..100、512KiB候选文件、400字符行输出、list最多200目录项不因服务化改语义；不扩大成无TOCTOU或安全沙箱承诺。

经理负责Engine/Store契约时应把最终读准入从仅read_file扩为三读：同runtime锁内同步Store复核desired.enabled与项目当前权限。单Host进程锁保证管理顺序仍不足以豁免这个明确持久校验要求。旧Task/grant、取消、根身份和结果落库语义不改变；shell能力仍可访问文件，run_wasm仍受现行Engine策略拒绝，二者不计四服务in_flight。

## 具体遗漏反例与验收需求

1. 只扩manifest/FILE_TOOLS而保留services[2]：zip只注册两项，Host.start因ProvidesNotCovered失败；必须数组/生产与mock装配同步四项。
2. 只增加registry service而list/search仍在COMPAT：definitions会在禁用时仍出现这两工具，或错误分支退回WorkspaceService。须从compat完全移除，并在disabled/unloaded/failed下断言两者零admit及无definition。
3. 仅改状态affected_tools不改Engine三读持久复核/真实registry：看起来受管而实际仍是compat；用新list/search专用marker服务证明定义与执行来自registry，再用真实目录/内容验证生产服务。
4. 老测试以disabled后list_files作为compat正控，现在必须按新契约改成合法run_command准入/安全命令执行正控（保持原权限审批），不能直接删掉compat断言；run_wasm不可作为Engine真实成功正控，因为现行项目策略拒绝它。旧D f04、B n10_disable_first及公开tool_runtime相应用例均受影响。
5. 仅正常四effect，不测部分激活失败：残留list/search可能被误用。至少检查四key绑定/owner、effect4→0、两插件启动计数与顺序、consumer失败rollback、后续显式启用及新false修复历史失败。
6. 仅沿旧read lease计数测试不足以证明新两服务受计数；分别list/search claim先赢、disable先赢、drop/错误归零及混合多lease总计数；旧write单次副作用与持久审批回归保持。
7. 授权只读Task应仍能list/search（tools=true、write_scopes=[]），tools=false拒绝；越界、禁路径和真实Windows链接/根身份既有反例复用，不为服务化放宽Workspace策略。

## 文件所有权建议与测试清单

D可维持tool_runtime.rs/tests/tool_runtime.rs唯一写权；不需plugin_host/catalog/Cargo/lib/build改动。B需Engine三读准入、plugin_management状态固定列表/版本，以及原Engine/module与plugin_management测试适配；C需CLI固定列表/说明/版本校验和HTTP/CLI正负控（包含旧plugin_http只影响两工具的断言）。Workspace现有实现建议只读；若新反例确证缺陷，另列最小范围，不把已有边界问题混入此次服务化。静态搜索已定位effects_count=2、provider仅read/write、disabled list_files兼容正控等旧期望。

建议组件验收：四真实服务来源及权限、四key注册/0残留、初始disabled零activation、四类lease计数与停止顺序/失败恢复。联合验收由经理冻结：旧库disabled重启后list/search也拒绝、enabled后三读真实正控、管理GET/回放零协调、旧Task/grant/审批不回退、compat仅shell/wasm来源准确；C最终固定组合门禁另行派发。D本次未运行任何测试，不沿用NEXT-10数目作为NEXT-11证据。

本只读任务完成后报告/身份冻结；下一第一操作仅接经理新完整实现任务（基线/唯一写权/Git模式/执行ID/验收），不能据建议自行开工。旧树与产物保留。
