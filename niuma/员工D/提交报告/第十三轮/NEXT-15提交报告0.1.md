# NEXT-15 员工D提交报告 0.1：真实安全输入首候选

日期：2026-09-28；作者：员工D / GPT-6-astra。执行ID：D-R13-01-20260928-E01。

本次是用户正式派发的同任务手动接续；原400 subagent未执行，不另开重复任务、不跨对话发消息。状态：**输入组件已实现并验证，首候选提交待经理审查/串行整合；完整D任务未完成，不自行APPROVED。**

## 1. 接收、基线与候选

已读取主仓根入口、D当前身份、项目开发/Git/路径规范、项目书现行目标、经理最新总进度，以及[NEXT-15契约v1.0含补充01](../../../项目经理/任务/2026-09-28NEXT-15安全路由与凭据初始化契约.md)、[最新执行登记](../../../项目经理/任务/2026-09-28NEXT-15执行登记.md)和[本人正式任务](../../任务/第十三轮/NEXT-15正式任务.md)。旧状态不覆盖本次有效派发。

本人源码树：仓库根相对 ../nhc-d-credential-input；分支 codex/d/credential-input。源码提交执行人D；主仓index、档案提交和所有跨树merge仅经理。

| 版本 | 完整HEAD | NoManCode tree | Rust tree |
|---|---|---|---|
| 本人实际接收基线 | 5194099ee4d1eb72af5f286826c72c6fd19d29e8 | f48df8d01f64095ad32b1412ac5a4db45b849182 | dfdedbfda40aba9502f1606cc466f2f32aa4e3d0 |
| 首候选 | 3675e3afeebb7c6ce73200a888588775bc55e3e9 | d0a79d25547e01d11d2b5117b6739423353292b5 | 44af58b76b067b7e34a29689ebe9c9f720a52309 |

接收时HEAD、branch、两层tree均与派发一致，status/index空；无继承未提交源码。经理后续管理提交未用于reset或擅自merge。

候选提交：feat(credentials): read bounded secrets without console echo。相对基线恰好两个授权文件：

- NoManCode/rust-app/src/credential_input.rs：真实输入、冻结类型、RAII恢复及模块内组件测试。
- NoManCode/rust-app/Cargo.toml：windows-sys仅追加Win32_System_Console feature；没有依赖升级。

lib.rs、B/C核心源码、现有测试及产品Cargo.lock均未改。两个Host集成测试文件尚未创建，避免用未接入的假桩冒充I04/I08/I09完成。

## 2. 实现与交接接口

源码行号针对上述固定候选。

| 项目 | 入口 | 实际行为 |
|---|---|---|
| 冻结API | src/credential_input.rs:11–75 | CredentialInputMode::{Prompt,Stdin}；SecretInput无Debug/Display/Clone/Serialize；CredentialInputError::{Cancelled,Unavailable,Invalid}；read_credential(mode)。 |
| 消费秘密 | :19–35 | C可调用secret.into_string()或String::from(secret)。未消费SecretInput和内部缓冲尽力清零；消费后String由调用方负责，不能宣称OS/依赖零副本。 |
| stdin | :61、:108、:172 | 先拒绝TTY；Windows额外只允许GetFileType为PIPE/DISK，未知设备不能因terminal探测失败而被接受。固定4098字节上限，读取至EOF，去掉至多一个末尾LF/CRLF，拒绝剩余多行、空白、控制字符、坏UTF-8和超长。 |
| Prompt | :189、:207、:310、:343 | 直接打开CONIN$/CONOUT$，不使用重定向stdin作后备。关闭ECHO/LINE/PROCESSED/VT input及Quick Edit后复核模式；真实ReadConsoleInputW读取UTF-16。提示只写真实控制台，不向stdout输出秘密。 |
| 取消与恢复 | :232、:259、:310 | Ctrl+C/D/Z、Esc作为取消输入；空Enter无有效凭据。所有正常返回路径显式恢复并核验原模式；Drop覆盖提前返回和panic展开。清空剩余输入，防拒绝的秘密尾部进入下一次shell读取。恢复失败覆盖成功结果，返回Unavailable。 |
| 多行与边界 | :289–340 | 拒绝Enter时已经排队的额外文本/多行，只允许单个配对LF；重复Enter拒绝。UTF-16有界，最终按UTF-8字节验证1..=4095。 |
| 固定错误 | :39–55 | 仅固定安全文本，不携带底层I/O错误、秘密或payload。 |

stdin的“空EOF拒绝”和“非空无换行末行可用”分开处理，后者依据契约“去掉至多一个末尾LF/CRLF”。为检测第一行后的数据，重定向生产者须关闭管道；字节有界不代表等待时间有界。Prompt以用户Enter为提交边界，未宣称预测此后才发生的键盘输入；正常返回/panic恢复证据不冒称强制进程终止后仍能执行RAII。

没有读取真实用户Key、环境凭据、DPAPI或产品数据库；所有输入验证仅用测试内合成数据。单独输入模块没有Host请求能力，但**C完整CLI“输入错误无PUT”尚未在组合上验证**。

## 3. 组件验证方式与原始命令

B尚未整合真实文件并唯一添加lib导出。本轮使用本人树忽略目录 .local/credential-input-component/Cargo.toml 的lib.path直接指向同一生产文件，不复制或改写实现、不添加产品stub，不作为完整peachsh编译证据。

隔离manifest仅依赖精确windows-sys 0.59.0与所需Win32 features；Cargo全程offline。已逐包核对隔离锁文件10项依赖均存在于产品Cargo.lock同版本，产品Cargo.lock未变。工具读取源为既有Rust/MSVC/SDK；MSVC通过只读探测既有安装目录定位，未安装/升级/迁移。工程、target、TMP、TEMP、所有原日志及发布副本均E盘。

实际命令工作目录：本人树NoManCode/rust-app。以下路径按项目规范相对化，未相对化的原始参数数组、UTC、环境、源码文件hash及退出码保存在原件JSON；发布副本见证据目录。

~~~powershell
pwsh -NoProfile -File ../../.local/credential-input-component/run-component.ps1 -Label attempt1 -Action test
pwsh -NoProfile -File ../../.local/credential-input-component/run-component.ps1 -Label attempt2 -Action test
pwsh -NoProfile -File ../../.local/credential-input-component/run-component.ps1 -Label attempt3 -Action test
pwsh -NoProfile -File ../../.local/credential-input-component/run-component.ps1 -Label clippy-attempt1 -Action clippy
pwsh -NoProfile -File ../../.local/credential-input-component/run-component.ps1 -Label candidate-format -Action format
~~~

runner实际分别执行：

~~~powershell
cargo test --manifest-path ../../.local/credential-input-component/Cargo.toml --offline -- --test-threads=1
cargo clippy --manifest-path ../../.local/credential-input-component/Cargo.toml --offline --all-targets -- -D warnings
rustfmt --edition 2024 --check src/credential_input.rs
~~~

runner读取既有build.ps1的环境发现部分，仅设置当前进程环境，没有执行build/release动作或修改该脚本。测试/dev debug=0、incremental=0、jobs=2。Rust/cargo实际版本1.98.1，rustfmt1.9.0，从Rust工作目录执行edition2024；不以根目录旧工具结果代替。

| 动作 | 实际退出码 | 结果/证据 |
|---|---|---|
| 环境attempt1 | 1 | MSVC自动发现失败，cargo尚未启动，未编译/未测试。environment-attempt1.json是工具结果转录，不伪称原始stderr字节文件。 |
| 组件attempt2 | Cargo/runner 101；外层工具进程报告1 | 编译成功；9通过/1失败/0忽略。无窗口子进程并不等于无控制台，拒绝Prompt夹具超时；已kill+wait超时夹具。另有测试内多余unsafe warning。保留原log/meta。 |
| 组件attempt3 | 0 | 10通过/0失败/0忽略；含2个无参数时空跑的子进程辅助入口，扣除后8个顶层实质检查。8次真实pipe/detached子进程，另一个私有控制台子进程内部执行30组Prompt案例和2次panic恢复检查。 |
| 限定Clippy | 0 | 真生产文件作为隔离crate，--all-targets -D warnings通过；不是整个workspace Clippy。 |
| 候选单文件格式 | 0 | 固定提交后edition2024 rustfmt --check；不是最终workspace格式门禁。 |
| git diff --check / staged --check | 0 | 限定源码差异；提交前读取完整staged diff，范围恰好两文件。 |

attempt2修复只改变测试夹具：无控制台用DETACHED_PROCESS并实核GetConsoleProcessList=0，保留正常pipe用例；删除无意义unsafe。未放宽产品输入校验。attempt3与Clippy发生在基线HEAD上的工作区候选，固定提交后对credential_input.rs、Cargo.toml、Cargo.lock逐字节SHA256复核一致，故不冒充它们的运行时HEAD已经是新提交，也不重复计数。

另有准备期两次JS载荷SyntaxError和一次路径转义导致OS267；均在shell/编辑启动前失败，无产品修改或测试结果，见preparation-tool-failures.json工具转录。早期只读探测的无匹配退出不计入产品测试。

## 4. 已证明与未执行的验收项

| 契约项 | 本轮真实证据 | 当前结论 |
|---|---|---|
| I04输入子项 | UTF-8、UTF-16错误，空白/控制字符/多行拒绝，4095/4096字节边界，恒流固定读取上限，I/O异常不返回半条秘密；公开API真实pipe与无控制台拒绝。 | 输入组件通过；完整CLI无PUT、DPAPI/环境/数据库反例待组合。 |
| I04 Prompt | 创建隐藏私有console，先确认仅测试子进程拥有；使用WriteConsoleInputW注入，实核ECHO关闭；两种初始模式、成功/取消/无效/超长及panic恢复；读取真实屏幕缓冲确认无秘密回显，检查队列清空和stdout/stderr无输入。 | 真实Console证据，不是终端模拟器/字符串mock，也未操作用户交互console。 |
| I05配置不授权 | 无B真实Host整合。 | 未执行；不能从输入模块无网络推断Host保存不授权。 |
| I08响应泄露与未知 | 输入组件子进程输出已检；无C配置CLI和B持久Host组合。 | 完整human/JSON/stderr、bootstrap token、恶意响应、stdout失败及lookup在途语义均未执行。 |
| I09真崩溃 | 无B事务停点与真实Host组合。 | 未执行。输入夹具超时kill+wait不是Host crash证据；无drop Arc伪装。 |
| I10/I11 | 无固定组合/最终放行。 | 未执行；最终五门禁仍仅C获经理放行后执行。 |

## 5. 经理/原owner需要衔接的具体事项

1. 可先审查并整合本真实模块至B/C；B唯一在lib.rs添加pub mod credential_input，C按冻结API调用并消费into_string。本人不改其文件。整合前使用本报告当前冻结回执，合后由本人实核新HEAD/两tree/status/index。
2. B完整Host合入后，D在tests/host_configuration_security.rs补：replace清key_env防环境覆盖、keep兼容和base变化拒绝、损坏DPAPI fail closed、secret/raw digest不落DB/WAL/日志/receipt、namespace保全；检查保存/回放/查询不发Provider、不改变grant/budget unknown。
3. C必要入口合入后，在D唯一测试文件补完整PUT后丢响应、恶意2xx含当次secret/token、超大/截断/重定向/错receipt与stdout失败；每个反例同时断言只发一次、operation_result_unknown、exit1、retryable=false；lookup404不作为新key重发依据。发现缺陷给经理入口/反例，由B/C修产品代码。
4. B提供真实事务commit前及commit后响应前可观察停点/子进程入口；D在tests/host_configuration_crash.rs通过ready确认后kill+wait实际Host进程，重启同一数据库两次，分别验证全旧无receipt或全新唯一receipt、revision/secret/grant一致及Provider计数0。
5. **测试接线需求（非已发现的B缺陷）**：契约要求B停点仅cfg(test)，而普通Cargo integration target中的依赖库通常不带cfg(test)。请经理/B冻结可由真实单测子进程调用的停点/启动入口或D测试文件注册方式；不为方便联测将故障开关暴露进生产HTTP/env，也不由D越权改lib/Engine/Store。停点必须在真正事务写入后commit前、以及commit之后返回响应之前，不能由测试名或构造数据库代替。

这五项是同任务后续依赖与组件计划，不是新任务派发、额外许可请求或完整验收声明。

## 6. 原件、发布副本与冻结

证据[manifest.json](NEXT-15证据0.1/manifest.json)列18项原件/发布文件的长度及双SHA256。原件位于本人E树.local/credential-input-component，原失败和子进程stdout/stderr保留；发布副本仅明确路径相对化/UTF-8发布表示，未重写结果。两份工具转录明确origin_kind；没有把转录当原始流。发布runner/manifest是原隔离入口归档，复验时按原件位置使用，不从报告目录直接运行相对路径脚本。

固定候选主要文件SHA256：

| 文件 | SHA256 |
|---|---|
| src/credential_input.rs | 7F414E74E16ABB40C81981F3900F483944B673D2C43A5C2D674A2D5DC7445101 |
| Cargo.toml | 622D465A7C3B542B71D8CD31F092E1240E13A9DCC452D1B92125722D964DBB68 |
| Cargo.lock（未变） | D8C5CECFD62A03EDB49EF29F18B2BE6063A2B27D7AD292BCB63B11F8D7AB8519 |

本人冻结实核：2026-09-28T14:17:10.3595223Z（北京时间22:17:10）。HEAD与两tree如第1节，source/status/index全部clean、no-build； scoped进程检查无本人构建或fixture进程，所有已启动组件命令已结束。见[candidate-freeze.json](NEXT-15证据0.1/logs/candidate-freeze.json)。本报告和身份完成后保持源码/本人本轮档案冻结，供经理审查归档及串行整合。

下一第一操作：收到经理通过用户转交的合并结果后，先实际核本人新HEAD、NoManCode/Rust tree、status/index；再依据真实B/C API与停点续做D安全/真crash组件。未release、付费live、push、跨树merge、reset/clean/stash或清理旧树；未触碰主仓index；未自动跨对话通知。
