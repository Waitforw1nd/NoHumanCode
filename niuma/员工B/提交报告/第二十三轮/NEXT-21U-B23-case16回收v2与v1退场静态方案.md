# B23 case16 回收 v2 与 v1 退场：静态待审

更新人：员工B Jason，2026-11-02。原matrix exec 26580及v1恢复exec 47108空poll均仍运行；case16仍unknown，15例通过、17–20未执行。原两gate保留，不创建任何请求、不运行新工具、不终止或清锁。

## v2候选

独立包位于 `nhc-b-process-lifetime/NoManCode/rust-app/.local/b23-case16-recovery-v2-prepared/`，未修改v1。清单SHA256 `af1853a85d9172c5094ea79dbf031b687bf97f642cce49de553418cea88e667e`。文件字节/SHA256：

- GateProcess.cs：3909，`367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42`
- recover.cpp：22181，`542bd950912f139621f0b5f8881149a4127dc2c7f0ac8f000274f02c1e31f83f`
- launch.ps1：13415，`71c190bcdd139c36649f1aa376b3c44f5094a0e529aea984d27eed13332f71d7`
- README.md：7614，`14cf8d852b598f3b913c68b3c941834f9549981817944a41f0c9136ed951f5d7`

对源副本、类型、helper PID/creation/wait/exit/image、Job accounting/limits/成员关系分项记录成功及错误；首次和重复检查分别调用源核验及Job/helper核验。helper映像查询失败明确记作缺失，不单独阻断；若成功但路径不符则拒绝，其他精确creation、从原case源复制及对象比较、固定probe hash/allow回执、Job关系与计数/flags条件仍必须全成立。这不能追认v1当时究竟哪个子项失败。

`00-abort.request` 仅在预检、未进入目标终止且另行授权时可用：重新核四位原owner活着及身份路径、case槽284为同一Job，逐一从原owner/原槽复制核所有已持副本相同，再复核owner和Job；全部成功才逐个关闭本工具自己的副本、核验副本和进程句柄，自退0。外层要求原工具signal/exit0、完成标记、无TERMINATED才认可。未取得Job、源变化、请求冲突、查询或关闭失败保留尚有句柄；部分关闭不可回滚。检查与关闭非原子，不能证明last-close或排除并发变化。C++未编译/执行；PowerShell Parser零错误不证明行为。仅供经理逐源码审查，编译、启动、abort及各终止阶段均须另行签发。

## v1定点退场方案（仅设计，尚无执行许可）

v1恢复工具PID44296、creation FILETIME 134381061967575472，原launcher PID88428、creation 134381061952164785，exec 47108。上级PID42264是共享Codex app-server，不对其发控制字符或终止。v1 stderr/stdout和原现场须先固化；确认没有01–05请求、stdout无任何TERMINATED记录，不能仅以进程名或PID证明对象。

1. 先由另签并经审核的限定诊断取得并保持v1和launcher精确PID/creation/映像/活着的process对象；从launcher的原process handle集合中用PSS定位候选、复制并CompareObjectHandles至该v1对象，记录查询、walk、复制及失败计数。核原case 89780/134381029069968888及runner/supervisor/launcher的原对象、活着和owner关系仍成立。
2. 证明v1副本与case原Job同一对象：仅针对v1进程的ProcessHandleInformation，使用诊断自身两个空Job动态校准类型索引、读取有限句柄表，筛选候选并DuplicateHandle与NtQueryObject核类型；从活case源槽284复制Job，与v1 Job候选以CompareObjectHandles比较，复核源槽与case身份。逐项记错误、数量与对象引用。不得全系统扫描或按数值直接认定。额外诊断副本须有自己的持有/释放计划；内部ABI、扫描与复制竞态、对象引用和异常崩溃风险须先审核，不能单凭采样宣称last-close安全。
3. 若经理单独批准定点终止v1且以上事实仍可重证，使用保持的v1原process对象而非PID重开执行TerminateProcess，等原对象signal并读真实exit；保存操作与原launcher生成的diagnostic.json（实际late_signal/exit，以原件为准）。该操作会由系统关闭v1所有句柄，可能涉及最后引用；即便重证case持Job，也不是原子保证，风险须经理明确裁定。任何条件不符或未知即保持停窗，不继续。
4. v1 launcher对非零退出会进入Hold-ActiveAfterException并持续持有原v1句柄，普通stdin并非退出通道。仅在步骤3原launcher证据已落地并经理**再次单独批准**后，重新验证launcher 88428的精确creation、路径及原process对象，对该对象定点终止并等待signal/真实exit；随后空poll exec47108取得实际终态，原文件与gate保留。若launcher记录缺失或对象身份无法重证则停止；不终止共享42264，不使用Ctrl-C。

此方案不改变matrix链和case16结果，不授权创建诊断、阶段请求或退场工具；两次定点终止及所有潜在引用释放必须另审。原v1预检错误为复合短路，不能推断helper映像不可读就是实际原因。

产品HEAD `98cec2e441adcb7cd0476c81db075c8f1ce4659e`、产品树 `85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e`、Rust树 `951cff6854e0a8bc0b7cc48d559b735c60256c21`，状态clean、index空。未运行Cargo、probe、测试或新诊断。
