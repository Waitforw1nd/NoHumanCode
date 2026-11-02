# B23 case16原进程句柄表诊断静态候选

2026-11-02，员工B Jason。v2诊断[release原始结果](NEXT-21U-B23-case16-PSS-v2-release结果.md)：6个源复核均匹配，18查询副本、6验证副本及诊断carrier共25次close成功，RELEASE_COMPLETE；原diag92080 signal/exit0，原exec16881因已记录空stdout异常exit1。原matrix session26580与两gate仍保留，unknown不撤销。

本轮只静态准备员工树Rust `.local/b23-case16-nt-handle-diagnostic-prepared/`，未编译、执行或查询carrier。完整文件：

| 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `case16_nt.cpp` | 22918 | `09f8c7a451015d295c47865f2a707cb9f9a5854bf0b5e1e3ac97a5a7e8375a28` |
| `launch.ps1` | 11711 | `f2cca005df735286c1015e0cf6653a26999f0962575d8875ca87ae7aab4a41f0` |
| `README.md` | 3605 | `429059dca67d29e8973966ebe1590d65278d1c234752970b553d7e57f9b3a8d4` |
| `manifest.json` | 778 | `f9b4994bc183c477956a50c2a8a7e6c71487312a266171545c4d39fc7239a6e6` |

## 范围与依据

固定本机SDK10.0.22621.0 `winternl.h` 可见NtQueryInformationProcess/NtQueryObject声明，但不含ProcessHandleInformation及其表结构。经理与本人核[System Informer phnt原始声明](https://raw.githubusercontent.com/winsiderss/systeminformer/master/phnt/include/ntpsapi.h)：该类位于ProcessWindowInformation(50)之后，x64每项为HANDLE、SIZE_T、SIZE_T、ACCESS_MASK及三个ULONG，表头两个ULONG_PTR；源码按class51、x64结构offset16/entry40静态断言，表长、数量、NTSTATUS和缓冲重试设上界。该声明是第三方内部接口实现资料；微软明确[NtQueryInformationProcess为可能变化的内部接口](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntqueryinformationprocess)，不能作为公开稳定承诺。仅本事故诊断使用，不引入产品依赖。

诊断自身创建两个匿名空Job，不关联任何进程，并取得自身process实handle；先以[NtQueryObject ObjectTypeInformation](https://learn.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntqueryobject)验证这三个已知自身对象类型，再以自身ProcessHandleInformation表定位其handle值，要求两个Job得到相同非零ObjectTypeIndex且与Process索引不同。只对同一carrier PID89780/creation134381029069968888原打开对象采A/B/C三次表，筛选与现场校准Job/Process index相符的条目，再DuplicateHandle、CompareObjectHandles及对副本实际NtQueryObject类型复验，随后才查询Job accounting/extended flags和process身份/等待/退出与IsProcessInJob关系。无固定类型号、不全系统扫描、不先以UNKNOWN/socket作试错，不读目标内存、不注入/暂停/终止、不更改Job配置；helper不按PID重开。

自校准得到的索引跨进程含义仍属未公开实现，采样与复制之间可发生槽复用，可能出现不同类型对象；副本事后类型验证不能回溯消除复制本身的引用计数副作用。ABA、角色与栈归属、Job active与last-close均不能由本次观察直接证明。结构、调用权限、ReturnLength/NTSTATUS不符合预期或校准不一致均停，不猜数值/升级权限。源码按v2保留专属release通道、逐源重复制比较及原对象监督；真实执行/诊断副本release分别须经理授权。旧v2 stdout首次空值的`.Contains`监控异常，已在此新包中显式将null转空字符串，原v2文件和事故保留。

PowerShell Parser零错误；C++未编译，未启动任何替代探针，未更改产品或原矩阵对象。原exec26580仍运行、两gate存在。经理下一步审非公开ABI、竞争和外围，再裁定有限诊断窗，不自动执行。
