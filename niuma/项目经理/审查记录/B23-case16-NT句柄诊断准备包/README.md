# case16 per-process handle-table诊断：未编译、未运行

本候选仅供审阅，不授权启动；原矩阵session26580、case16、两gate不受控制。`NtQueryInformationProcess(ProcessHandleInformation=51)` 只针对已登记case16 carrier的原打开对象及诊断自身，不调用系统范围handle枚举、读目标虚拟内存、注入、暂停或终止，不以PID重开helper。微软公开文档将NtQueryInformationProcess及返回结构视为随版本变化的内部接口，公开SDK的winternl.h声明了函数，但本机SDK10.0.22621.0未给出该51信息类及handle snapshot结构；不能将这里的结构/行为称为微软公开稳定契约。源码x64结构布局、class51来源为System Informer项目phnt声明（见员工报告），经sizeof/offset静态断言、长度/数量上限检查及诊断进程内自校准；仍未在本机编译或实测。API从系统目录已加载ntdll.dll动态解析，未找到/布局或长度异常/NTSTATUS失败则停止、记录，不升级权限或换系统范围扫描。

诊断自身新建两个匿名空Job（不设置限额、不分配任何进程），复制自身process伪handle为真实handle；对这些已知自身对象调用NtQueryObject类型名核`Job`和`Process`，再对自身ProcessHandleInformation表精确查这三份handle值，要求两个Job相同的非零ObjectTypeIndex、process为不同非零值。索引每次现场校准，不写死类型号；它跨进程语义在公开契约中未获保证，不能因此证明目标槽类型。自身Job仅用于识别，绝不是原Job。

对同一carrier对象先核PID/creation/Wait(0)，做A/B两次per-process句柄表，仅将与自校准Job/Process index相等的槽当候选，并输出每轮发现数及其余数；相同槽/类型才DuplicateHandle，C采样复核并通过CompareObjectHandles。对复制所得对象再用NtQueryObject(ObjectTypeInformation)验证类型名，然后才查Job basic accounting/extended LimitFlags、Process原对象GetProcessTimes/Wait(0)/exit及IsProcessInJob关系；失败/不匹配均记录并保留副本，不作成功。不会先复制表里UNKNOWN或其他类型作为试错；采样后槽仍可能复用为socket等类型，目标槽类型筛选和副本事后核类型不能从理论上消除这段竞态。不得将候选、单次active=0、Wait/exit或这套未公开ABI的观测单独升级成park栈归属/原Job终态/last-close证据。

保留v2同一attempt专属`release.request`：查询完默认持所有诊断副本，经理审结果并明确授权后才可创建文件；同载体PID/creation/活着核验，逐源再复制并与每个已持副本CompareObjectHandles，全部通过并再次确认carrier后才只关闭诊断副本。对源槽变化/失败保留并报unknown；此过程非原子，不保证最后引用不会并发变化。两个空校准Job从未关联进程，仅在诊断正常退出时由OS回收。外部杀死/崩溃仍可能导致诊断副本关闭，不可掩盖该风险。

`launch.ps1` 从v2修改诊断源/EXE名称、核ntdll存在与哈希，修正首次空stdout为null时`.Contains`异常：先将null明确变为空字符串；异常active原handle保留分支和120秒后持有不重试原则保留。独立E盘新attempt的TEMP/TMP/obj/exe/log，固定MSVC14.43 Hostx86/x64与SDK10.0.22621.0；不争抢原两gate。manifest、源码和脚本仅作待审静态准备；C++未编译、脚本未运行、未查询carrier。经理须审非公开ABI、类型筛选竞态、调用范围、源码与监督逻辑，另行授权特定诊断和随后的release。
