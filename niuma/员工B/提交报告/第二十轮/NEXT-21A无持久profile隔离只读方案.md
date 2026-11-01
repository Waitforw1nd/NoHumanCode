# NEXT-21A 无持久 profile 隔离只读方案

2026-11-01，员工 B。基线及边界反例为 [attempt8](NEXT-21A-A0-attempt8真实profile反例.md)；此文仅提出可证伪方案，未修改产品、ACL、系统配置，未执行动态实验。目标仍是无用户资料、项目、Host状态或不受控自身可写存储访问，保留LPAC、零capability、创建时Job、句柄白名单及实际token鉴别；不得把可写profile重新列为允许例外。

## 已知与未知

本机Windows SDK 10.0.22621.0 的 `userenv.h` 声明 `DeriveAppContainerSidFromAppContainerName(name, &sid)` 从名称派生SID，成功SID须 `FreeSid`；同时声明 `CreateAppContainerProfile`、`DeleteAppContainerProfile` 和 `GetAppContainerFolderPath` 为不同API。SDK头只证明声明、参数和注释，不证明派生身份无需profile即可成功启动，也不证明系统不会按SID自动生成目录或注册表存储。`winbase.h` 声明 `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` 及ALL_APPLICATION_PACKAGES opt-out属性，`winnt.h` 声明 `SECURITY_CAPABILITIES` 包含SID和能力数组；这些也不证明无profile路径的安全行为。经理已核Microsoft官方Create/Derive/Delete文档：Create涉及每用户每应用文件夹与注册表；Derive页面说明名称派生SID但未保证无profile启动；Delete要求关闭句柄且失败后状态不确定。不得将这些声明推导成已经支持或安全。

## 首选候选：仅派生唯一身份，不主动创建profile

Host为每次受控调用生成不可预测、规范化且独立的名称，通过受支持的 `DeriveAppContainerSidFromAppContainerName` 得到SID；失败或SID无效即 `isolation_unavailable`。不调用CreateAppContainerProfile，也不先创建再删除profile；由经验证的工件/专用工作目录按原授权赋该SID必要只读加载权限，沿用实际Windows系统加载例外、`SECURITY_CAPABILITIES` 零能力、LPAC opt-out、Job-list原子归属、三句柄白名单、挂起后实际token/SID/完整性/Job/AccessCheck核验；仅全部成功且在期限内才resume。保留明确的SystemRoot及本次工作目录LOCALAPPDATA，不能继承真实用户环境。SID生命周期只释放内存，不能据此假设profile不存在；派生、访问控制和启动相容性及上述环境是否仍有效都待实测。

在每个步骤由Host用本次SID查询相关profile位置，记录API真实HRESULT、返回路径（若有）、目标存在性及类型，并在创建前、挂起时、resume后、退出/回收后分别复核；返回路径仅是位置证据，API失败/目标不存在都不是权限拒绝。若Windows自动出现关联目录，必须在仅确认本次新SID对应且无预先存在对象的条件下，为本次目录提供与attempt8等价的Host无秘密哨兵，由相同受限probe直接测试原哨兵读取/写入和新建写入，Host记录前后摘要与是否存在；一旦成功访问即是反例。若目录不存在，测试应记录实际路径不可用和直接创建/打开的原始结果，而不能将路径不存在或无法解析标成ACL拒绝；仍须审查其他能由该身份到达的目录、注册表和自身存储，无法确认则该边界未验证、不得报通过。为避免挂起和创建交界竞态，采用有界Host监督停点：进程创建后仍挂起时观测并建立必要哨兵，resume后再测；无证据支持“启动不会自动创建”。是否能在不扩大允许访问或预先人为创建被测存储的前提下形成完备反例，是实验的明确通过前置。

真实profile注册表不能用子进程GetAppContainerRegistryLocation返回拒绝替代；需另设计同本次SID绑定、Host可验证的实际注册表位置及受控哨兵，并区分位置不可证实、路径不存在、API失败和明确拒绝。这个方法尚未确定，现阶段是阻塞项；不得假设SID无profile即无注册表或其他身份可写空间。

## 同入口敏感性和重启验证

- 正常启动正控：相同显式构建的probe、相同加载路径、Job/管道/token核验和S01通信必须真实完成，记录二进制与输入hash、实际token SID/能力/完整性、AccessCheck三项、PID/Job和退出；假如无profile身份启动或loader失败，应记实际Win32码和未启动，不能当隔离成功，也不能回退普通进程。
- 检测灵敏度负控：在单独新建的、仅供对照的传统profile上沿attempt8相同入口与同样固定哨兵操作，要求确实检测到直接访问、摘要改变及新文件；正控与候选使用独立SID、独立Job及受控清理。若负控无法复现，不以候选未观察到访问推断安全。普通AC的AAP AccessCheck负控继续保留，区分LPAC语义与profile哨兵灵敏度。
- 被测无profile路径须记录创建前/后状态、受限进程的每个动作原码、Host独立摘要及回收事实；一旦其实际可读/写、产生未授权存储，或安全检查无法区分环境错误和拒绝，按边界反例/未验证停止。网络仍须真实受控listener及原权限语义，不因WSAStartup 10107而判拒绝；同时保留Host进程/Job控制、文件、注册表、资源、超时、强杀和工件矩阵。
- 创建一个可审计、仅含随机身份与本次staging关联的持久回收清单；在取消、超时、正常结束、Host强杀及Host重启后由外部观察者持真实进程句柄验证进程退出与Job归零，并记录SID关联资源与原目录前后状态。清理仅限可证明是本次创建的工件，不能对其他profile或用户文件作广泛清理；发现由系统自动生成而不能安全归属/回收的对象应报告未解决并拒绝继续。不得把 `FreeSid` 或 Delete API调用本身当成存储已回收的证明；强杀各交界与同步初始化硬期限须单独验收。

## 备选与停点

若派生SID无profile路线无法启动、系统自动产生可写profile或无法有界证明/回收，不能用删除profile后直接启动并猜测系统不重建来规避；应保持失败关闭并由经理选择后续核心顺序。也可评估独立受限身份与profile分离，但必须先取得受支持API文档及实际token/资源命名语义，证明子进程身份无权访问其任何实际用户或自身存储，并以attempt8同路径直接访问反例和普通AC/传统profile正负控验证。当前对这种分离没有可声称可行的证据，不建议直接实现，也不使用未证明的native低盒接口或修改真实profile ACL。

本方案没有为潜在方案、额外系统运行库或网络权限申请扩大例外。实施及首次动态需经理另行冻结实验契约；本轮不编译、不运行、不改产品。
