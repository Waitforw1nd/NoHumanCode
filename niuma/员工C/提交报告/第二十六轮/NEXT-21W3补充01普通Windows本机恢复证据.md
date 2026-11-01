# NEXT-21W3 补充01：普通 Windows 本机启动边界证据

2026-11-01 · 员工 C / 6.1 Terra，第二十六轮只读返工。替代原报告将外部签名平台作为缺句柄恢复必要出口的建议；不重复命名 Job 和持久绑定的整体设计。本机算法尚未实现/实验，不代表现有 W2 已支持跨崩溃解封。

## 事实、权限和适用边界

1. Microsoft [Acquiring high-resolution time stamps](https://learn.microsoft.com/en-us/windows/win32/sysinfo/acquiring-high-resolution-time-stamps) 正文 FAQ 明确 QPC 非递减；不同线程取得的值相差 ±1 tick 时次序有歧义。正文说明 `QueryPerformanceCounter` 是 Windows 启动以来的 ticks、包括 sleep/hibernate，`QueryPerformanceFrequency` 在一次启动期间固定，64 位计数至少 100 年才可能回绕。另一官方文档 [GetTickCount64](https://learn.microsoft.com/en-us/windows/win32/api/sysinfoapi/nf-sysinfoapi-gettickcount64) 说返回系统启动以来的毫秒、分辨率通常 10–16ms，不受系统时钟调整；它不是启动唯一 ID，文档并未把“双计数下降”直接规定为恢复协议。两 API 是本机普通 Win32 查询，无需新增 registry 写入权限或网络 capability；若平台/API不可用、返回无效或部署不能建立其语义则保持 unknown。
2. 本方案依赖受信 Windows 内核/Host/数据库、无恶意管理员或篡改 OS/数据库/系统标识，且严格**同一实际机器、同一安装、同一数据域、无机器克隆、VM 快照回滚、数据库旧备份恢复/回滚**。这是部署与恢复契约，不是靠一次时间比较自动证实的事实。特别是复制 VM/整盘可能复制机器 GUID、数据卷和数据库，因此机器 ID 或文件 ID 也不是不可克隆证明；无法排除迁移/克隆/回滚时必须关闭自动解封。未授权的 caller 不能通过提交布尔值或两个数字触发通过；恢复器自行读取 OS、锁与持久记录。
3. 对身份保守绑定：实现定义的安装 ID 应在首次初始化持久记录，并由本机受信安装配置及 DB 双方关联；恢复期间通过受控句柄核数据目录/数据库所在卷及文件身份、非重解析、同一锁对象与已记录域一致，读取本机受信安装标识，失败/权限拒绝/变化拒绝。`GetFileInformationByHandle` 等需本机对目标句柄具备查询权限；对机器标识的选取、ACL 及安装方式须实现后验收，不能声称任意 Windows 机器名称或 MachineGuid 自身能防克隆。允许本机通常用户读计数；若产品选择把安装标识放受保护 HKLM，需预设管理安装/ACL，查询仅在 ACL 允许时工作，本方案不要求修改 HKLM。数据库通过原路径替换、迁移或合法恢复时保守提示人工重新建立证据，不把身份不符当新启动。
4. [RegCreateKeyExW](https://learn.microsoft.com/en-us/windows/win32/api/winreg/nf-winreg-regcreatekeyexw) 说明 `REG_OPTION_VOLATILE` 在对应 hive unload 后消失，HKLM 对应完整关机，且 key 已存在时该 flag 被忽略；用户选择关机默认可能是快速启动。[Windows 启动模式](https://learn.microsoft.com/en-us/windows-hardware/drivers/kernel/distinguishing-fast-startup-from-wake-from-hibernation) 区分冷启动、休眠恢复和快速启动。HKLM volatile 可能需要管理员预先创建父键和精确 ACL，key/value 也可能被删除、权限拒绝、建立失败或 hive 改变；**键不存在/不同绝不能单独解除**。建议第一版不依赖 HKLM volatile；若日后用它作辅助，记录 disposition/权限/错误、只作额外一致性提示，仍不得把缺失当证明。

## 推荐算法：两个严格下降是充分而非必要条件

记 `Q()` 为 QPC 原始正整数，`F()` 为 QPC frequency，`T()` 为 GetTickCount64 毫秒。按受支持 Windows、上述信任和不回滚条件，一次 OS 启动内 QPC 非递减，系统 uptime 也应符合系统启动以来计数的语义；同机每次采样顺序固定 QPC 后 T。若持久前值与恢复后值满足下述**双下降**，且实现经过支持版本/休眠/重启测试，则可作为发生过一次系统启动边界的保守证据。频率只用于一致性与安全裕度，不把 QPC 当跨机器绝对时钟，不要求重启后 QPC 必然清零。

### 启动登记

- 持有能覆盖所有 Host/Store/worker 启动入口的同一数据域排他锁；校验安装/机器/文件身份与数据位置。旧 `main.rs` 的锁没有覆盖所有 `Store::open`，这是一项未来实施前提。
- 对新的 worker，先在 IMMEDIATE 事务中封装可核对的 `request_id`、task/binding generation、owner epoch、状态版本、机器/安装/库身份，标记 `creating` 并记录 `q_before = Q()`、`f_before = F()`、`t_before = T()`，提交持久完成后才允许调用 OS 创建。记录从可信进程内直接取得；无效值、失败或提交失败均不能发起创建。采样点应在**创建前**且 gate/owner 尚有效；此后任一创建中的崩溃都可用该前值作为候选重启基准。所有调用必须受同一 owner/门控制，禁止新 boot 自动运行旧 worker 或由持久代理/子进程脱离受控创建。
- 原 Host 仍须在 resume 前把已取得真实 process handle 的 PID/creation time 和对应代持久登记；同 boot 的恢复继续遵守原报告的进程句柄信号、创建/提交系统调用已结算及单 worker/无后代等条件，不因为两个时间字段存在就放宽。

### 可选提高检查点与人为正常重启

缺句柄且未知时，新恢复器取得独占、数据库 gate 关新创建、更新 owner epoch 围栏、核可信安装/数据身份后，可在 SQLite IMMEDIATE 中对同一 unknown 操作记录更新一组 `checkpoint=(q_cp,f_cp,t_cp)` 与 state version，并记清来自**当前**恢复器、采样时的 gate/owner 和 request 集合。此检查点只表示在此刻这些 unknown 已归入封门的恢复集合；不冒充旧 Host 对同一 boot 的观测。即使先前创建调用是否已结算不明，检查点后所有旧 OS 中的执行和创建尝试仍会被真正发生的 OS 启动边界消除；前提是不会有持久自动启动者在新 boot 无视 gate 重建工作。更新检查点前后不得把旧 worker 标 completed。

用户可通过 Windows 正常“重新启动”操作，而非仅注销、应用重启、睡眠或休眠；本方案不需要 Host 持有关机特权或调用重启 API。为提高可恢复性，UI 可在 checkpoint 前等待本机 uptime 足够长，使重启后登录耗时通常小于它；这只是改善成功率，不是退出证明。若 checkpoint 太小或重启后新 uptime 已超过 checkpoint，不能用本方法证明：可在封门条件下重新准备检查点，再安排一次重启；任何 API 查询失败则停留 unknown。

### 新 Host 事务与证据

新 Host 重新核真实数据域锁令牌与安装/机器/库身份，确保无克隆/迁移/回滚风险；保持所有 worker gate 关闭，优先完成恢复核验，不先启动 worker。读取受信 DB 中请求 checkpoint/状态版本及关联集合，自行顺序采样 `q_now=Q()`、`f_now=F()`、`t_now=T()`。下列全部成立，才允许进入提交：

```text
q_cp > 0, q_now >= 0, f_cp > 0, f_now == f_cp
T 和 Q 的计算无负数、溢出、回绕、读取失败或异常来源
q_now + q_margin < q_cp   // q_margin > 1 tick；使用 checked 算术
 t_now + t_margin < t_cp  // 至少大于公开计时分辨率，建议 >=100 ms
checkpoint、request、owner/generation 和关闭的 gate 仍与所读一致
可信同机/同安装/同数据库、无克隆或回滚、所有创建入口均受围栏
```

建议 `q_margin=max(2, ceil(f_cp/1000))` ticks；计算上限、加法与转换均 checked，溢出 fail closed；毫秒阈值 100 是保守设计值，不是证明系统最大误差的官方保证，目标机支持边界仍须实验。如果任一值不下降、频率不等、采样隔离身份不可验证、时钟源/平台语义不在支持范围，结论是 **unknown**，不是同 boot，也不是完成。下降只能作为发生启动边界的充分候选，不能作为必要条件或无条件 boot ID。

若成立，受信恢复器将**checkpoint 之前已归入该封门域的旧 OS worker 和旧 OS 内未完成创建**视为已越过启动边界；这里不要求事先证明旧 boot 里的每个创建调用已结算——启动本身消除了旧 OS 的线程与调用。仅在封门、无新 boot 自启动/逃逸、原子 Job 与无后代假设成立的范围内有效。再以 IMMEDIATE 事务按 `(operation_id, request_id, binding_generation, owner_epoch, state_version, checkpoint_version, gate_closed, domain_identity)` 逐条 CAS `cleanup_unknown → completed`，记录采样事实及恢复方法、审计和幂等回执；并发 CAS 冲突或新记录未列入检查点保持 unknown。此前已 claim 的审批、文件、网络、Shell 和 Provider 效果保留原 unknown 语义，绝不因本证明重放或宣告成功。

正常服务不应只在用户完成重启后才创建 checkpoint；否则若仅有当前时点数值，就无法证明此前发生过什么。机器已启动很久、旧启动时刻 checkpoint 的 uptime 已低于当前 uptime 时，同样只能重新封门、建立新 checkpoint，再经历另一受支持重启。耗时之外不应提供手动强制清除数据库的旁路。

## 正式实验与失败解释

| 场景 | 必须取得的证据和结果 |
| --- | --- |
| 非管理员普通桌面会话 | QPC/频率/GetTickCount64 查询正常；机器和目录身份读取成功；拒绝权限时无自动放行，记录 API/错误，不默认修改 HKLM |
| 真实创建前/creating 中/登记后 Host kill | 验证创建前采样已持久与原代关联；新 Host 关门及提高 checkpoint 可运行；旧 worker 和创建交界均进入 unknown，不依靠创建调用已返回 |
| 真正正常 Windows 重启 | 旧 boot 的受信观察者若在退出前已取得 worker 真实 process handle 信号，可单独保存当次观测；若未观测，不得补称 wait 通过。真实 process handle 不能跨 OS 启动保留或在新 boot 等待，此观测不是跨 boot 恢复前置。分别保存旧 boot 日志及真实重启模式证据；新 Host 按同安装/库自行采样 QPC/F/uptime，核对持久 checkpoint、gate 和 CAS。双下降及全部前提满足才可完成；不下降或频率改变则安全阻塞并登记可用性失败 |
| sleep、hibernate、快速启动、单纯重启 Host | 对每一模式记录是否有跨边界及 worker 是否存活；普通睡眠/休眠恢复若 worker 存活绝不可触发双下降放行；快速启动不能预设等于冷启动，按真实模式和 API 观察，不以 volatile 键消失代替实验 |
| VM 克隆、快照回滚、不同设备复制数据库、文件替换/路径别名 | 显式不支持/必须外部标记关闭自动恢复；即使 GUID、路径、计数碰巧一致或下降，未能排除克隆回滚也不能通过；锁须统一真实数据域身份 |
| 计数误差、频率变化、整数边界、旧版本/错误回执 | ±1 tick、阈值内下降、checked 溢出、错误 API、不同频率、过期 checkpoint/epoch、CAS 竞争均 unknown；双下降但 DB gate/代变化也不通过 |
| checkpoint 后旧创建者迟到及新 boot 自动执行 | 使用实际受控停点核旧 OS 进程及线程随真正重启结束；另测新 boot 所有可创建入口均受锁与 gate 约束；若存在逃逸/外部持久发起者，不得发布此算法 |
| 重启失败/过慢或仅当前可见值 | 无下降或不够大仅表证据不足；提供封门后新 checkpoint + 再重启的路径，不强制改状态 |

## 基线及未验证事项

本补充采用已核收的第二十六轮主仓 `8128f5c135ba0ae96ba8d2be40710e6fdc81d1a4` / 产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58` / Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`；正文引用 Microsoft 官方页面及本地代码，不声称这些接口已在本机实测。**没有写注册表、改权限、重启、运行产品或实验。** 本机启动边界判定仍是可实现候选；需要先落实全入口独占与禁逃逸、安装/库身份及回滚策略、跨启动清理状态机，再在受支持 Windows 版本上完成真实模式矩阵。W2 组件可继续，不将本报告当 W2 动态验收或完整恢复已通过。
