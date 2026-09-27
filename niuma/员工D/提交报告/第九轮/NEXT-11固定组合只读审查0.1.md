# D-R9-01 NEXT-11固定组合只读审查0.1

2026-09-27，员工D GPT-6-astra，原执行ID D-R9-01-20260927-E01。经理明确追加有界只读review；固定组合af4d291e9cde1ca578e7c621e0984f5cacc9122a，实核NoManCode08cd78a59cad53f390c826c7484d21e3b820abed、Rustef2d8775209065ceb11a34890feed50afa7e2e3b。全部shell主仓显式活动盘cwd，仅git show/diff固定对象；没有产品修改、测试、merge或其它员工档案操作。固定组合D两源码与9a10c724无差异。

**限定结论：指定三读准入、B L03/L05测试设计、CLI双版本和真Host重启增量未发现新增阻断或可确认假通过。下列证据边界保留，不能扩大为已执行测试/最终L08通过。**

- 三读Allow分支在tool_runtime.acquire持锁callback内统一validate_files_read，后者Store锁内先desired.enabled再当前项目/根绑定权限，未新增Store→runtime反向调用。B n11_three_reads测试特意只更改持久desired、保持registry active，三种execute_or_wait均精确typed Unavailable、业务全表不变/in_flight0，能排除仅由definition关闭造成的假通过。权限收紧部分直接acquire+Store callback精确PermissionRevoked，属于最终准入组件证据；不是并发撤权时序实测，但明确生产分支使用同一函数，没有发现绕过。
- L03分别list/search取得真实生产lease后，以5秒oneshot双向停点安排持久禁用，再执行真实perform_tool，观察in_flight1/draining/effect0，落库真实结果/单条对应tool_result、审批数不增，计数归零及原文件不变。其Task来自已完成fixture并补入assistant tool call，测试直接调用acquire/perform_tool，**不是一次由Provider驱动的完整在途新Run**；应仅称Engine组件持久结果链。实际Provider路径由tool_session_turns恶意调用负控与C真Host启用独立Run补足。按本契约允许的组件/联合分工，该边界不构成阻断。
- L05以真实非空Task/两次写/恢复历史建立schema12，种入NEXT-10同结构immutable receipt与desired真/假两分支，重开新Engine并逐值比所有表（含管理表与migration），核version1.1、revision/time不漂移。enabled分支先仅内存disable，再原key回放current必须保持stopped，因此确能抓到错误reconcile。旧restore Task resume精确Sealed、全表及文件保持。**它是旧schema12表示兼容重开，不运行旧1.0二进制，不是crash**；unknown/grant不重试的完整原测试由最终回归继续覆盖，不能称该新L05单例单独生成了unknown故障。
- CLI只接受1.0.0/1.1.0两种精确affected/compat数组，未知版本与任一字段跨版混搭/任意工具名均拒绝；human打印已校验DTO，JSON原样；PUT解析拒绝仍映射operation_result_unknown且不重发。新子进程测试分别验证两个版本list/status/set/human，并以request计数检查拒绝后无重发。1.0是受控旧DTO响应，**证明协议映射兼容，不代表实跑旧Host二进制**。
- C L06每个list/search独立临时Host/Provider/DB：实际child.kill+wait非成功退出后重新launch；启动期Provider零请求、desired原样/admission关/effect0/provider空/全DB不变；disabled真实Provider恶意调用精确未授权failed、零tool消息/事件、累计Provider1；显式enable后另一run start真工具结果，list精确a.txt、search精确路径/行/正文，累计Provider3且defs相符。不是仅名字断言或错误分支冒绿；原read场景也保留。它不能观察启动期短暂activation，零激活由D固定组件相同装配路径证据负责。
- B旧disabled list_files compat正控没有直接删除后留空：另新增run_command真实新Run，先无ack审批精确拒绝/NotStarted，再settle显式approve+network ack→Finished/marker/ack表1行，并验证files仍关闭；wasm保持失败/无审批/无工具事件。不存在将component callback当真实审批的混淆。

残余范围：未实跑C/B当前候选，不认领其定向统计或日志hash；本次也未重复审迁移实现、shell沙箱或Workspace算法。仅根据固定源码确认设计链路与上述限定证据一致；最终L08仍由C固定组合五门禁及经理联合验收决定。D原053e515 Clippy101和新9a10通过证据保持，不改旧报告。

当前报告/身份写毕冻结，源码继续9a10c724不变。下一第一操作仅接经理固定差异review或结项通知；不自行新开测试/实现。
