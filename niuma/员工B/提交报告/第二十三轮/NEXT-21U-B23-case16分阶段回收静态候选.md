# B23 case16分阶段定点回收静态候选

2026-11-02，员工B Jason。NT诊断[release结果](NEXT-21U-B23-case16-NT-release结果.md)原diag49760 signal/exit0、exec1729 exit0；原matrix session26580和两gate继续持有，case16事故unknown保留。本报告只是准备回收方案，**未编译/运行本包，未终止或释放原矩阵对象、未清gate、未写阶段请求文件**。

新包位于员工树Rust `.local/b23-case16-recovery-prepared/`：

| 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `recover.cpp` | 14159 | `1a5238c47e2cedd9b813e221bee40ddf6462abd5e6d8fd5d28153f243a2a5b00` |
| `launch.ps1` | 12965 | `10c826589f31d05acac9b3a0cb7daebe1a2e39e3ff43bcee76351eb66245c5e2` |
| `README.md` | 6347 | `02dfc457fe51015a03e9fba640e3ebca0862df543f786c67251dfea1e3f9e909` |
| `manifest.json` | 755 | `477cbfd95a33358551bfd88b730974ecdbe2c1b9c14c6d49f335513925d8cade` |

## 对象与证据责任

只读CIM观察链89780→62476→87376→90776→42264；90776是原matrix launch脚本PowerShell，42264是Codex app-server宿主，不在终止范围。CIM时间精度截断，另用进程对象读取精确创建FILETIME：case89780=`134381029069968888`，runner62476=`134381028898031298`，supervisor87376=`134381028875429933`，launcher90776=`134381028866695431`。最后一项为现场只读观察，不是原launcher事前签发身份；源码仍须GetProcessTimes/可读映像路径再核，并从launcher所持process handle复制、CompareObjectHandles到原supervisor对象，不能凭CIM/PID独立决定。

代码固定四身份，只打开这四个目标对象；载体源handle284 Job、296 helper process各取两副本核真实类型/同对象，helper由原handle取得PID91236/FILETIME134381029070411070、可读probe映像、signal/exit0，Job复核active0/total2/flags0x2800及helper成员；case终止前从源槽重复制并对所持Job/helper作对象比较。PSS限定父进程handle元数据，分别从runner复制比对case、从supervisor比对runner、从launcher比对supervisor；任何一项无法确认便保留对象停。观察到的active0与源码park、原对象关系用于本事故窄处置，不能证明任意外部工作、IO或last-close。

## 提议步骤，均须另行授权

工具默认停在PREFLIGHT_READY；各独立attempt专属请求文件由经理审上一步后决定是否创建：`01-case.request`仅定点TerminateProcess已核case原对象、等原handle signal及Job仍0；`02-runner.request`定点结束runner并验证supervisor真实生成的runner `incident-late-signal.json`；`03-supervisor.request`定点结束supervisor并验证原launcher真实生成的 `launcher-late-signal.json`；`04-launcher.request`才在已核launcher PID/精确creation/映像及持有者关系前提下定点结束原launcher、等原handle signal；原exec26580实际终态须再由原session空poll取得，可能非零。原Hold-Original无限等待、并不读取stdin，普通字符不是控制通道，Ctrl-C可能影响整棵链，均不用；绝不结束上级Codex app-server。各步不证实即保持对象并停，不自动重试。

原runner/launcher终止会关闭其持有的gate OS句柄，但代码不删除或移动gate文件；原gate文件/证据应留待经理后续核处。`05-release-diagnostic-objects.request`独立请求仅供未来审定，默认保留工具持有的Job及其他副本；若执行，其Job副本可能为最后引用并触发kill-on-close，即使active0也不能证明IO收束，须明确单独裁定。源码不调用TerminateJobObject、不清gate、不按名字批杀；已执行动作无法回滚。无法从原PSS诊断推断case16原业务成功或将15/20写成矩阵通过。

外围仅拟E盘独立attempt下固定MSVC/SDK编译并用原process handle监督恢复工具；检测PREFLIGHT_READY、超时保留、无自动阶段文件，PowerShell Parser零错误，C++未编译。源码对已退出helper映像路径的QueryFullProcessImageName若失败会安全停窗，不能预言一定可用；PSS内部采样竞争、复合条件和launcher异常保留路径需经理逐行审查。下一步仅经理审源码/安全边界并分别签具体阶段；本次未执行任何终止。
