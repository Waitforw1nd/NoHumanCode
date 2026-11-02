# B23 case16分阶段定点回收：v2 静态待审，未授权执行

本包含`TerminateProcess`，**仅供经理静审**；不得编译、启动、创建stage请求或终止任何进程，除非经理后续逐项明确签发。目标是保全和核对象后回收本次事故链，不将原case16未知改成通过，不清/迁两gate文件，不调用TerminateJobObject、不按名称批杀、不得Ctrl-C。

## 固定对象与取得责任

`recover.cpp` 固定case 89780 / FILETIME134381029069968888、runner 62476 / 134381028898031298、supervisor 87376 / 134381028875429933、launcher 90776 / 134381028866695431；GetProcessTimes、PID、可读映像路径、Wait(0)逐一核，失败停止。runner/supervisor/launcher映像期望为启动恢复工具的`$PSHOME/pwsh.exe`（或powershell.exe），case为原矩阵`matrix/fixed/lib-test.exe`；外围核case/probe文件SHA与原构建一致。helper不按PID重开，保持case对象后只从其原handle槽296复制并核PID91236/FILETIME134381029070411070、signal/exit0；helper映像查询成功时必须匹配probe路径，查询失败单独记录缺失和错误码，不以此单项阻断；Job只从原carrier槽284复制。对284、296各取两个副本用CompareObjectHandles核同对象，并NtQueryObject核Job/Process真实类型；再次从carrier源槽复制与所持对象比较，核Job basic active0/total2、原flags0x2800及IsProcessInJob(helper,Job)=1，两次有间隔核对且终止case前再核。源槽数值本身不是证明；这些条件/已park源码只能支持本次窄处置，不构成ABA、所有IO、last-close或整个U证明。PSS/复制间仍可能槽复用；候选handle增加引用并一直保留。

原所有者关系为强制条件：PssCaptureSnapshot只取父进程handle元数据及process类型专属PID，从runner的现存process handle复制并CompareObjectHandles到已核case对象；从supervisor复制比对runner，从launcher复制比对supervisor；若父PSS失败、无匹配、出现候选比较失败、对象/时间/路径不符则保留全部对象、报告unknown，不执行任何终止。PSS不扫全系统，不读目标虚拟内存/线程上下文，不重开helper PID。复制异常、活跃原进程路径不可读同样停；已退出helper路径不可读仅记证据缺失，仍须满足固定probe文件hash与allow回执、精确creation、原槽同对象、Job关联和计数/flags全部条件；PSS采样/复制竞态和通过打开case槽进行即时dup造成的额外引用须经理知悉。

## 预检显式退出与限制

`00-abort.request` 仅在预检和首阶段请求之前接受，必须另经经理明确授权。工具对case、runner、supervisor、launcher原句柄再次核身份、路径及活着，从case槽284重新复制并比较Job；对自身每个已保留副本重新从其原owner/槽复制并CompareObjectHandles，再次核四个owner及Job。核对全成后只关闭本工具持有的副本、核验副本和本工具打开的原进程句柄，逐项记录close并自退0；不关闭原owner句柄，不终止目标。外围需看到原恢复工具signal/exit0、PREFLIGHT_ABORT_COMPLETE且无TERMINATED记录才认正常退出。条件不足（含Job尚未取得）、源变化、重复请求冲突、关闭错误则停并保留尚持句柄；部分close不可回滚。复核和close非原子，目标并发变化或恢复工具异常退出可改变引用，不能声称last-close证明。`01-case`进入终止之前禁用abort；不得同时送请求。

## 单向阶段及请求文件

外围生成全新E盘独立attempt，工具默认停在`PREFLIGHT_READY`，不自动创建下列任何文件；每个请求均须经理审上一阶段结果后另行裁定：

1. `01-case.request`：重核case/runner活着、case源槽仍与所持Job/helper为同对象、Job/helper条件仍成立；只对case原对象TerminateProcess(code 0xB23)，等该原process handle signal、读回code，再以仍持Job/helper副本重核Job active0/total2/flags及helper身份/terminal/关联。失败保持全部对象，永不继续。
2. `02-runner.request`：重核runner/supervisor及Job/helper，定点TerminateProcess runner并等原handle signal/exit；然后只等待原supervisor自身生成的 `incident-late-signal.json`，核其中runner PID/creation、原handle late exit query成功/exit code与直接读到的exit一致并存在时间；无记录或不匹配30秒后保留未知。记录由原supervisor产生，工具不伪造。
3. `03-supervisor.request`：重核supervisor/launcher、Job/helper后定点TerminateProcess supervisor，等原handle signal/exit，并等待原launcher生成的 `launcher-late-signal.json`，以同样条件核supervisor原对象身份及exit。无证据即停。
4. `04-launcher.request`：只有原launcher late-signal已证后，重核launcher PID/精确creation/映像及Job/helper，定点TerminateProcess **已核原launcher对象**并等待其原handle signal/exit。原session26580的实际返回需由持有者以空poll读取；与前述signal是两条记录，可能为非零，不能预写。runner被终止时其持有的inner gate OS句柄、launcher被终止时common gate OS句柄会随进程关闭，但对应gate文件应保留，代码不删除或迁移；若文件状态不符须停并另审。
5. `05-release-diagnostic-objects.request`：工具默认继续持Job、helper及所有核验副本。仅经理另行审核所有阶段及gate后，可授权最后重核Job/helper及四个process已signal/指定exit，再关闭本工具自身副本和原process handle后退出。case进程已终止，此时本工具的Job副本可能是最后引用，关闭可能触发kill-on-close；即使active0也不是IO或last-close证明。若经理不接受该风险，保持持有，不能自动进入本步。任何查询/比较/等待/终止/关闭失败保留尚持对象并输出RECOVERY_UNKNOWN；已发生的终止或close不可回滚。

## 原launcher与stdin边界

只读CIM观察父链89780→62476→87376→90776，其上级42264为Codex `app-server`宿主，可能服务其他工作，绝不终止或向它发控制字符；CIM把FILETIME末位截断，不能作精确身份凭据。另以进程对象StartTime取得90776精确FILETIME `134381028866695431`，其命令行为原session启动的matrix `launch.ps1`。新工具还必须从该原launcher持有的process handle复制并Compare到指定supervisor对象，不能单凭这次CIM或PID。原`Hold-Original`在late-signal写完后无限Wait/Sleep，不读取stdin，也没有正常完成的现成命令通道；向exec发送普通字符无法证明被处理，Ctrl-C可能影响整棵控制链，均不使用。阶段4是对已核原launcher对象的定点终止设计，而非假设stdin可使它正常退出；是否允许及原exec宿主关系仍须经理在执行前确认。

## 外围与限制

`launch.ps1`仅准备E盘独立attempt、固定MSVC/SDK编译以及以原process/thread handle监督恢复工具；不申请/解除两个matrix gate、不自动生成请求文件。120秒未到PREFLIGHT_READY记录timeout并保留原恢复工具对象，unknown/监控异常持有并停，不能猜杀；实际诊断过程、Job副本因异常退出可能被系统关闭。源码静态检查和PowerShell parser不能代替编译或行为证明。内部NtQueryObject只用于所复制候选类型名，未公开内部接口/缓冲及PSS竞态需经理审核。此包没有执行许可，所有输出、obj/exe/TMP未来均E，不安装工具；动态回收、后续修复和矩阵重跑均另行签发。
