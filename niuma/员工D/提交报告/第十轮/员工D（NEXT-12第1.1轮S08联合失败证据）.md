# 员工D NEXT-12第1.1轮S08联合失败证据

2026-09-27，员工D GPT-6-astra；D-R10-01-20260927-E01，同任务接续。经理授权只在本人tool_runtime模块增加真实shell生命周期故障联合证据，B提供cfg(test) Engine::new_with_runtime_for_test；未改B源码/生产接口。

固定提交5e74afc7f808963c7a9ca7c568dd2bd78ac3a5ab；NoManCode7f768071f0943aa160624ee31a947e9843c48746/Rustb8da46c8de86c7e942a140b7d22c9ae78ad00e19。唯一增量src/tool_runtime.rs测试78行，源码/index空、所有构建结束，已报经理干净整合窗口。独立树../nhc-d-shell-service，员工提交本人源码、跨树及主仓index仍仅经理。

## 基线接续与协调事实

先实核经理合B的e6bcb6d76284da03d1899cbddaa3a5f2b4ee451e/NoManCodeea53bd9d11c4b53b345f5974d5434a15494a1542/Rust592068819049f5aeb20fc16668437f9ced0d6bc0。经理随后在未再次取得D无构建确认时合B352a968，为fa2c7dda95f2b822fb34e0fe135163934d363ec0；经理说明协调失误。实际核对merge提交UTC05:13:23，D写入并check开始UTC05:15:02，故本次构建不跨merge；实际执行基线为fa2c7dda（NoManCoded4997aebe5127ad984fc3aa69c8d6fd2a5ef83e3/Rusta8d23dd0aa189ac29fac69bba1eccb92c9ba851b）加D新测试。最终5e74afc固定后再次执行S08确认。未自行merge、未覆盖他人修改。

## S08结果与范围

真实ShellProbe成功激活后，deactivate返回错误。经真实Engine::set_plugin_enabled(shell,false,rev0,key)得到accepted receipt rev1、desired=false，而actual Failed、effect0、不收敛。独立SQLite连接逐值验证shell desired的enabled/revision/updated_at与receipt的ID/enabled/expected/result/committed_at确已持久提交。

将故障模式恢复正常后，同key旧请求回放receipt/整个current完全相等，实际仍Failed，证明不调用reconcile；新key同false显式协调，rev2且Unloaded收敛。随后真实缺少run_command注册的activation失败仍持久接受true/rev3并显示actual Failed；修复故障模式后的旧key回放不协调，新key true/rev3形成rev4并恢复真实1effect，recovery历史保留。

files完整PluginStatus与独立SQL enabled/revision/updated_at从始至终不变，四条receipt全部属于shell、files receipt0。S08不跨访问Engine私有runtime、不绕审批执行文件工具；另一组真实四工具正控引用第1.0 S02，二者组合说明隔离，不冒称本例再次通过Engine执行了四文件工具。

检查均exit0：同一源码原build.ps1 check；固定5e74afc的S08单测1通过/0失败/0忽略；cargo clippy --locked --lib --test tool_runtime -- -D warnings；本人tool_runtime.rs rustfmt check（原日志0字节）。首运行1/0与固定重复确认不累计成2个用例。新增10对原始/发布日志及元数据hash见[D-R10-01-S08新增证据清单](D-R10-01-S08新增证据清单.json)，原第1.0的26对manifest不改，合计36对。UTC/命令/退出码按元数据；target/TMP/TEMP/log仍仅新E树.local，debug0/incremental0/jobs2，机器MSVC只读，旧证据产物保留。

本轮只重验限定新增S08及相应检查，未重复此前34组件、未跑最终五门禁；B/C及经理证据不归为D执行。D授权实现与当前补充完成，无当前D阻塞；源码和档案冻结待经理review/最终组合明确复核。下一第一操作只接经理明确审查/返工，不自行继续扩实现。