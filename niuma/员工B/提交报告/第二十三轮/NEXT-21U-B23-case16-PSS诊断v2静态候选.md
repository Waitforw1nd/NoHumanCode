# B23 case16 PSS诊断v2静态候选

2026-11-02，员工B Jason。经理读v1后要求Job limit、完整计数、可控release及真实编译/监督脚本，并指出Start成功后的日志/监控异常须持原handle。现新建员工树Rust `.local/b23-case16-pss-diagnostic-v2-prepared/`，原v1和矩阵现场不改；本包未编译/执行，不含诊断结果，不创建release文件。

| 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `case16_pss.cpp` | 19951 | `1432f90a15b84e0a81ab2068c3f6e72b0fe2d1550e184ed7895b11947bd70209` |
| `launch.ps1` | 11369 | `487475c5b5571592e30e0918194526e05fec15b55992c5aab1584b9adb480ab2` |
| `README.md` | 5749 | `70a687a48e378c02d59e91bb5efef4491f14a98d6c705b8d301951256f3e0930` |
| `manifest.json` | 744 | `a75eb01a8f0c53f11175aa69edfd0295424a1ab4d2a7446fdc72983c6bd58e29` |

PSS限case16 carrier的handles/name/type-specific信息，三次身份/候选采样，显式计各类型、无法辨类型、采样不一致、duplicate/compare/identity/query失败与最终候选；Job仅明确类型名时查basic accounting及ExtendedLimitInformation的原始LimitFlags、breakaway/silent-breakaway/kill-on-close，逐项记录成功与失败。PSS/复制不能原子绑定、job识别可能缺失；不以candidate关系冒充栈对象证明。

诊断正常输出`QUERY_COMPLETE`后默认持全部副本等该次attempt的`release.request`；文件由经理审结果后另行授权创建。见文件后用同一carrier再次核PID/creation/活着，对每个已复制的源handle重复制并将临时副本与该源已有所有副本比较，记录临时handle及结果，再核carrier，全部成立才关闭诊断自己的所有副本及carrier并输出`RELEASE_COMPLETE`；失败则保留未关闭原/副对象、临时副本并输出unknown。source关闭与最后比较之间不能原子锁定，仍不得声称last-close；如实际Close失败，之前已关闭的副本不可回滚。

`launch.ps1` 拟在E盘新attempt下设置TMP/TEMP、复制并核源码/manifest、使用固定MSVC/SDK编译，原`GateProcess`对编译器与诊断创建挂起隐藏原process并记录PID/creation。诊断120秒未完成查询则记录timeout、继续持原handle且不猜杀；正常路径允许release后确认原signal/exit并退出。编译/诊断Start返回后即置active；任一监控/日志异常进入最外层catch，尽力记录incident，写入失败也继续以原handle等待；诊断仅确认原signal、exit0及`RELEASE_COMPLETE`才清active/Dispose。准备包复制、Add-Type、编译发生前不接触原case；不争抢/解除矩阵两gate。启动函数内部若成功创建而未能返回handle、launcher外部崩溃仍是需经理知晓的有限边界。

关于经理提出的KernelBase.lib：已只读检索固定SDK10.0.22621.0全部Lib目录及MSVC14.43.34808目录，未找到该文件；固定SDK kernel32.Lib字节中也未见CompareObjectHandles符号。不能如请求将不存在的库加入固定link输入。源码改为在打开carrier前，从已加载的系统目录`KernelBase.dll`经GetModuleHandleW/GetProcAddress解析该函数并核模块路径，失败即停；launch检查该DLL存在并记录其SHA及kernel32.Lib SHA。此为待经理审的动态解析替代，不是已证明链接成功；不下载、生成或安装import lib。

PowerShell Parser零错误；manifest四文件长度/SHA核对一致；C++及脚本未编译/运行。原session26580仍运行，两gate存在，产品HEAD98ce、clean/index空。下一步经理静审本包并另签诊断，另审release文件创建；不自动开始、终止、重试或改变原unknown。
