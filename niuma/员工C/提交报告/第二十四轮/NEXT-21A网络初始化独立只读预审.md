# NEXT-21A：网络初始化独立只读预审

2026-11-01 · 员工 C / 6.1 Terra。仅审查；不运行探针，不修改产品或 B 的现场。

## 事实及证据边界

- 固定 B `83b2e3a7a262ad31f0ca23dfaef1c55af10a536c` 的 `NoManCode/rust-app/examples/external_process_probe.rs:61-71,119-146`，直接调用 `WSAStartup(0x0202, &data)` 并记录**函数返回值**；只有返回 0 才分别尝试 IPv4/IPv6 `TcpStream::connect_timeout` 与先 bind 再 send 的 UDP，随后 `WSACleanup`。现有记录合并 UDP bind/send 的错误、TCP 创建/连接阶段也未拆开；不能定位具体调用或资源。
- attempt7 `tmp/.tmpLem9NU/sandbox-stdout.bin` 记录 `network.wsa_startup` = 10107，确无三项网络动作条目，`external_network` 为 `not_tested`。同目录 `sandbox-result.json` 记录实际 token `app_container=true`、`less_privileged=true`、`capabilities=0`、`in_job=true`、integrity RID 4096；受限 probe 正常退出。`control-stdout.bin` 是**普通进程**同 probe 对本地 TCP v4/v6 和 UDP 的成功对照；固定测试 `tests/external_process_isolation.rs:255-288` 还确认 UDP 被本地接收。另一条 `ordinary_app_container_access_check_is_sensitive` 测的是普通 AC 的文件描述符 AccessCheck，不是网络正控。联合测试在 `missing tcp.ipv4` 处 exit 101；DNS 和外网均未验证。具体原件位于 B 树 `NoManCode/rust-app/.local/next21a-b-83b2e3a7-attempt7/`，详见 [B attempt7](../../../员工B/提交报告/第二十轮/NEXT-21A-A0-attempt7.md)。
- 固定环境的 Windows SDK 10.0.22621.0 `um/WinSock2.h:594,601,659` 定义 10013 为 WSAEACCES、10107 为 WSASYSCALLFAILURE；`shared/winerror.h:24884-24890` 给出的后者说明只是 system call failed。Microsoft [WSAStartup](https://learn.microsoft.com/en-us/windows/win32/api/winsock/nf-winsock-wsastartup) 文档说明：先成功启动才能使用后续 Winsock，失败码直接作为返回值，不能用 WSAGetLastError 替代；[Winsock 错误码](https://learn.microsoft.com/en-us/windows/win32/winsock/windows-sockets-error-codes-2) 指 10107 是多种情形下的泛化错误，举例包括协议/名称空间目录相关注册表调用失败，并非本次诊断结论。[socket](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-socket) 与 [connect](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-connect) 失败各应立即采 WSAGetLastError；不同 API 的同一个错误码含义依其上下文，不可预先把所有 10013 都当策略证明。
- Microsoft [Launch an AppContainer](https://learn.microsoft.com/en-us/windows/win32/secauthz/implementing-an-appcontainer) 说明 LPAC 与普通 AC 的资源授权差异、capability 与 DACL 双重约束，并指出没有网络 capability 的 AppContainer 不能访问网络；它没有给出本机 LPAC 零 capability 下 WSAStartup 一定成功或 10107 的原因，也未列出足以安全授予的完整初始化资源清单。本机 SDK 给出符号和签名，不能替代系统实现的动态证据。Microsoft 已读网页有实质正文；UWP capability 页面抓取未包含具体能力正文，本报告不依据它作细节推断。

**判断：**本轮唯一网络事实是受限进程初始化失败；它既不是执行了 socket/connect 后的策略拒绝，也不证明系统资源不可达是根因。初始化与协议目录/服务/文件/注册表访问、调用环境或系统异常可能相关，均只是待排除假设。未找到官方支持的“未成功 WSAStartup 便直接执行 Winsock socket/connect”的等价验证方式；绕过 Winsock 调用内部驱动/未文档化接口不能替代受支持路径。已经存在的父进程连接、继承 socket 或代理只能证明额外授予句柄的能力，不能证明零权限 worker 新建网络能力受限。

## 最小可证伪实验（建议经理交 B，未执行）

1. **原配置三组并行对照，先不改变权限。** 同一受控 probe 和二进制哈希、同一 OS、Winsock 版本、同一控制本地监听器及目标：普通进程正控、真实普通 AC 零 capability 对照、原始 LPAC 零 capability/Job 边界组。两种 AC 不得额外给网络 capability，记录从实际 token 读取的 AppContainer SID、LPAC 属性、capability 数/集合、完整性、Job、进程 PID/创建时刻、无网络句柄继承和 probe 哈希；同时标明普通 AC 与 LPAC 的其他启动差异，不能声称完全单变量。普通 AC 启动本身若失败，记为未建立对照，不能换成普通进程冒充。每次固定新鲜独立受控 fixture，并把控制连接与监听端的接收/接受按 nonce 和 PID、时间窗口关联；UDP send 成功不等于对端收到。
2. **逐层记录，不把未执行填成拒绝。** 每组首次调用 WSAStartup(2.2)，记录直接返回码、成功时 `wVersion/wHighVersion`；成功才分别执行原始 Winsock `socket(AF_INET/AF_INET6, SOCK_STREAM/SOCK_DGRAM)`、对应 `bind`/`connect` 或 `sendto`，每一步记录实际 API、返回值及失败时立即获取的 WSAGetLastError；逐项独立且每项都有明确 `not_executed`，关闭已创建 socket 并平衡 startup/cleanup。还保留 IPv4、IPv6、UDP、DNS、外网 S04 作为后续单项，不把三个 loopback 测试等价成全部覆盖。IPv6 fixture 不支持时为环境缺失。DNS 要实际解析、分清初始化/服务不可用/策略，外网使用经批准的可控目标与独立正控，不擅自产生真实外连。
3. **定位 10107，仍保持原 LPAC token。** 首先由受信的测试监督者读取记录系统版本、进程模块/provider 与可获得的协议目录信息，不把内部 DLL/注册表数据推断成授权需求。若有经授权的外部诊断机制（例如受信监督端以范围限制的 Procmon/ETW 采集，仅过滤该 probe PID、启动时间和系统初始化相关操作），记录事件来源、操作类型、规范资源标识、返回码与时间，并用普通进程和普通 AC 同法对照；工具可能需要管理权限，属于采集者的单独权限，不授予 worker。日志只保存最小脱敏元数据，不读取凭据或哨兵内容；采集不到或无拒绝事件不能证明没有其他原因。不得对生产 ACL、全局网络策略或 profile 动手。
4. **如何判定：**若原 LPAC 成功初始化、三类 socket 对应 API 真执行，普通进程与受控监听器证实目标可用，且受限错误及事件能对应同次策略边界，才可报告该 API/目标上的实际拒绝；先创建 socket 失败也须标清是 socket 创建而非 connect。若初始化仍 10107、系统资源缺失、超时、目标不可达、连接拒绝、监听未就绪或仅 WSAEACCES 而无可归因对照，均是未判定或仅通用访问失败；不得宣称网络隔离通过。跨组资源 ACL、token、地址和时间不同造成的差异需显式控制或列残余不确定。
5. **只有诊断证据指向特定初始化资源后，再请经理裁决最小增量试验。** 优先复查该资源对零 capability LPAC 的系统预设可访问性，不修改全局对象。若提出对象级只读/查询权限，须逐项列出确切对象、当前 ACL、拟授权的唯一测试 SID、精确访问位、影响、还原和审计；实际系统受保护对象可能不允许安全局部授权，无法做到时不实施。即便只读授权也扩大可读范围，不能称权限不变。`internetClient` 等网络 capability 会扩大网络权限，不能用于原始零 capability 的通过证据；若另行批准，仅作为独立阳性敏感性对照，不能归入原组结论。禁止默认授予继承 socket、Host 代理或任意系统资源访问权。

## 限制与交接

本次固定主仓 HEAD 为 `3842c7c5aee278f305c6956b486bfa8b3bd31ca7`，即指定基线；NoManCode tree `897cc8510c0c56118df04c2c1dbcea9c999e539f`、Rust tree `3672895f5b4083c3d0a7a09d9d520c4a9ceadada`，基线至 HEAD 产品 diff、工作区产品 diff、index 与 unmerged 检查为空；继承的人员文件保留。B 树固定文件和 attempt7 原件只读。本轮未构建、测试、运行 Host、网络或系统探针。下一步由经理审查后交 B 决定执行；当前网络策略拒绝、DNS、外网及完整 S04 均未证明。
