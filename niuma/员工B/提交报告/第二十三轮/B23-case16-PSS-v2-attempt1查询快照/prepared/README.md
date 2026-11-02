# case16 PSS诊断v2：静态候选，未获执行授权

保留原v1；v2只对已登记carrier PID89780/FILETIME134381029069968888拟做PSS handles/name/type-specific元数据，禁止VA clone/读取内存/线程注入/暂停/终止、按PID重开helper、修改Job flags或CLOSE_SOURCE。A/B/C采样及DuplicateHandle副本对象比较只能建立候选关系，不能排除ABA或证明park栈所有权。PSS无Job专用ObjectType；仅TypeName精确`Job`才尝试Job查询，未识别的对象不盲目复制。捕获的对象名不输出。每轮输出Process/Thread/Job/其他及无类型名数、候选，累计输出采样不一致、duplicate/compare/type-query失败、身份不匹配、复制与验证计数。通过的Job查询basic accounting与ExtendedLimitInformation的完整LimitFlags及breakaway/silent-breakaway/kill-on-close标志；每个API结果单独带成功标志，失败不能解释为0。

风险仍须审：PSS与复制间槽可能复用，实际复制对象可能不是PSS类型，甚至是微软警告复制会影响引用计数的socket。多次快照与CompareObjectHandles只能缩小竞争，不能把它称原子证明。对Job或误分类成其他类型的Job做副本持有会增加引用，无法作为last-close证据；诊断进程崩溃也可能关闭副本。无需先做系统范围探针。PSS不提供可辨Job类型名时即报告不可识别；未来fixture可主动登记原Job/进程对象作为另案替代，不追认当前现场。

## 默认持有与单独授权释放

查询结束输出`QUERY_COMPLETE`后保留carrier和全部诊断副本等待；没有自动写release文件。经理审阅实际输出并另行决定时，才可在该次全新attempt目录创建唯一`release.request`普通文件。检测到后，诊断先对同一个carrier handle重核PID/creation/Wait(0)，对每个已复制的源handle再DuplicateHandle为新的临时副本，记录临时副本值，与该源已持的每份副本用CompareObjectHandles逐一核同对象，全部成功后再次核carrier，才仅关闭诊断自身副本、验证临时副本和诊断自己的carrier handle，输出`RELEASE_COMPLETE`后退出；不关闭源、不终止进程。任一复核、复制或比较失败即输出`RELEASE_UNKNOWN`并保持所有尚持副本、含临时副本，不继续关闭。关闭调用本身失败也停并记录，先前已成功关闭的副本无法撤销。

此复核不是原子事务：源仍可能在最后比较后、诊断副本关闭之前被并发关闭；比较成功不保证不会改变最后引用，绝不宣称last-close或事故完成。若需要这一额外保证，必须由经理在release前另定持源协调/处置，而不能仅依赖文件通道。新PSS或类型查询失败、部分观察、无Job候选不应被理解为全体已核；输出完整计数供审。C++异常在可能已有副本时捕获并保持；进程外部强杀/崩溃、操作系统故障仍可能关闭它们。

## 待审启动外围

`launch.ps1`要求全新E盘Rust `.local`直接子目录与完整manifest SHA，复制并核四个prepared文件，先在attempt建立TMP/TEMP，再用固定MSVC14.43.34808 Hostx86/x64及SDK10.0.22621.0原位只读编译C++（object/exe/log均E）；Add-Type复用既有GateProcess。编译器和诊断各以CREATE_SUSPENDED|NO_WINDOW启动，由原process/thread handle取PID/creation、resume、Wait与exit并留日志。编译失败不启动诊断；编译观察超过300秒记录超时并继续持原handle等待自然signal，不猜杀。

固定SDK10.0.22621.0全部Lib目录和固定MSVC目录只读检索未发现`KernelBase.lib`，其`kernel32.Lib`字节中也未见`CompareObjectHandles`符号，不能伪称已加入缺失的静态import lib。v2因此不直接链接该符号：使用已加载的系统`KernelBase.dll`经`GetModuleHandleW`和`GetProcAddress`解析，核其实际模块路径等于系统目录下`KernelBase.dll`；解析或路径失败时在打开carrier之前退出。启动脚本检查该系统DLL存在并记录其SHA及SDK `kernel32.lib` SHA，仍只链接kernel32.lib；该替代尚未编译或运行，须经理明确审阅是否接受。未下载或生成import lib。

诊断启动后120秒未见`QUERY_COMPLETE`，记timeout并继续持原handle、不重启；如果后来完成查询，记录`query_complete_late`且超时事实sticky，仍可按经理明确指令使用release文件。正常query后脚本持续等待同一原handle，release成功后核`RELEASE_COMPLETE`和exit0；`RELEASE_UNKNOWN`继续持有，等待进一步处置，不把它标成成功。脚本无第二个独立永远park的正常层，唯一release通道由attempt内文件触发；低层原process wait/exit查询或创建身份异常时保留原handle和incident，不猜PID或强制结束。诊断是同一事故的只读观察，不申请、不清除矩阵两个gate；这些gate仍由原session持有。

编译器/诊断的Start成功后立即置active；此后所有监控、读取、元数据写入异常均进入最外层持有分支，尽力写incident，写失败也继续持原process handle并轮询。该分支对诊断仅在观察原handle signal、exit0及`RELEASE_COMPLETE`后才清active/Dispose；编译器也须先确认原handle signal和可读exit。读取日志持续失败或状态无法确认时继续保留并等经理处置，不因日志IO异常离开持有者。此保护不能覆盖启动函数在成功创建进程后、向调用者返回handle前的异常或外部杀死launcher，仍需经理审查。

脚本和源码均未编译、未运行；`launch.ps1`的实际manifest参数、专属attempt位置、stdout/进程状态、release文件创建时机须另行审核签发。当前不授权运行或释放。
