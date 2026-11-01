# NEXT-21W2 受控 WASM worker 进程生命周期设计

2026-11-01，员工 B；只读基线 `5b1c91da984283322e6907151b5e38dbbbb998ce`、产品 `7267b1187c5e46f6632cf546bc6ab8638d33a3b9`、Rust `fb9828e63c4f11c5633c83e570b761de6dfc438d`。未改产品或系统、未创建工作树、未构建或执行。本报告是供经理冻结实现的方案，不是W2验收。A的真实profile可写和派生SID启动失败均仍OPEN。

## 现状、信任与产品裁决

W1现有 `src/wasm_worker_protocol.rs` 是内存/同步流协议，`src/wasm_worker_executor.rs` 是worker侧wasmi执行器；并没有已接通的独立worker二进制或Host进程传输。三帧依序为大端u32长度+manifest JSON（<=4096）、input JSON（<=8192）、module（<=8 MiB）；接收完第三帧后仍要求stdin EOF。响应为一帧JSON（<=512 KiB）并要求stdout EOF。Host必须比较完整Manifest（含nonce、request ID、generation、digest等）并使用原Input做phase/decision检查，不能只比ID；`Outcome::Error`也不是决策。W1执行器在真实worker内验模块digest、拒绝imports、限制fuel/内存/表并检查导出。既有 `tool_call_application` 的 `run_wasm` 显式拒绝和PluginHost builtin闭集保持，不接Engine/Store/schema。

建议W2先验证**由Host发行并认证的固定可信worker运行无imports字节码**，不运行插件提供的native文件或程序路径。无imports使正常WASM没有直接Host文件/网络API，但wasmi和Rust worker是同用户native代码；若选普通同用户token，解释器/worker内存安全漏洞、动态库加载链、同用户进程和继承环境等仍可能暴露Host权限，Job不提供文件、注册表、网络隔离。此模式仅作明确标注的WASM组件可行性方案，**需要经理对同用户token与其实际权限单独裁决**；不能称A成功、不能成为任意native插件fallback、不能承诺恶意WASM突破解释器时仍受OS安全隔离。若产品威胁模型要求该防护，必须先有经反例验收的OS身份和加载边界，否则只能继续拒绝接入。环境至少明确`SystemRoot`和本次临时目录，禁继承全量环境、凭据及PATH；DLL搜索策略/系统runtime允许集合须另明确验证，不能笼统信任可写工作目录。

## 固定工件和一次请求

1. 单独构建 `wasm-policy-worker`，入口仅接受继承的stdin/stdout/stderr，拒绝argv路径、环境或manifest选可执行程序；worker只运行 `read_request -> execute -> Response -> write_response`，stdout只发协议，stderr禁止原文秘密。worker不得加载插件native代码、shell或把WASM字节路径重开。Host自身的可信发行信息固定worker版本和SHA-256（构建顺序为先worker再将release描述符/哈希以可信方式绑定Host；不能让可写旁文件自证）。
2. Host在调用准备开始取单调`deadline=now+5s`，从可信部署位置对该发行工件做独立快照；目录/文件句柄固定、逐组件拒绝reparse、拒绝多硬链接/可替换目标，按上限读哈希并保持禁止写/删除共享的映像句柄至创建后验证，按实际Windows语义核映像文件ID和预期哈希，悬挂进程启动前再次核ID；任何无法证明失败关闭。快照放Host独占、不可由不可信写者改名/修改的目录。必须审证预先存在的写句柄、可写映射、目录替换、DLL搜索和同用户权限，单靠哈希或一次路径检查不能声称防TOCTOU。发行信任根、安装/更新和snapshot清理合同须经理冻结；若固定可信发行不可保证，此项不可用。
3. 模块由调用方交一次有界字节快照，Host对该内存缓冲做长度和SHA-256，并在请求Manifest中绑定同一digest；写管道直接发送该缓冲，worker再次校验，不按用户路径重开。Host自身不解析不可信WASM。输入只含W1既有工具类别/轮次，不含消息、参数、凭据；请求字段/固定工件完全不受插件控制。序列化两个JSON帧并检查上限，模块可分块发送，避免复制多份8MiB；所有长度相加溢出拒绝。

## 所有权与管道

Host内部一个生命周期协调器拥有单次调用占位符、唯一Job句柄和process/thread句柄；Job `KILL_ON_JOB_CLOSE`、active-process=1、breakaway不启用、非继承，通过`PROC_THREAD_ATTRIBUTE_JOB_LIST`随`CreateProcessW`原子加入，`CREATE_SUSPENDED`后检实际Job与映像，再判断取消/期限后才允许恢复。仅stdin/stdout/stderr三个child端出现在`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`，Job和Host端不出现在列表；不可把Job复制给worker、测试外部观察者或任何独立监督进程。若Host本来处于限制不兼容的外部Job、创建属性不可用或实际归属无法证明，结构化失败关闭。初始128MiB process memory、每进程3秒CPU、总deadline 5秒、回收确认2秒都是待测试配置；CPU限额不等于墙钟限额，host准备与内核调用时间不因此自动有界。

Windows匿名`CreatePipe`句柄不能简单当overlapped管道。建议Host创建三对本机命名管道：Host端以`FILE_FLAG_OVERLAPPED`建管道并保留非继承句柄，child端由Host先连接并只将已打开句柄列入白名单；随机不可预测名称、首实例、拒绝远端、最小DACL、局部连接及完成连接后再启动。使用`CreateNamedPipeW/ConnectNamedPipe/CreateFileW`、overlapped `ReadFile/WriteFile`、事件或IOCP及`CancelIoEx`，连接和取消均处理同步成功、`ERROR_IO_PENDING`、连接竞态及真实最终完成；所有OVERLAPPED结构和buffer必须活到完成通知，调用CancelIoEx或close不等于I/O已完成。同用户攻击者的抢先连接/DACL能力并未凭随机名称自动消失：检查本机连接端身份/所有权、首实例及不向worker传管道名，加入抢占负控；若同用户竞争不能消除则只可在相应威胁假设下继续。Windows SDK声明这些API但这里未证明每个组合/ACL与取消行为，应按目标Windows动态实测。

协调器同时推进三路I/O和进程句柄，限制每路未完成操作与缓冲：stdin按3帧流式写完整8MiB及header；全部完成后关闭唯一写端产生EOF。stdout增量读4字节长度、验证非零/<=512KiB再接收恰好一帧，并继续读至EOF、拒绝额外字节；不能在响应帧完整后停止读，否则尾随字节/不关stdout会被漏掉。stderr并发drain，达到例如64KiB即终止worker并返回 `diagnostic_limit`；只保存固定类别、byte count及可选摘要，不能把不可信原文/模块/秘密写产品日志。读写同时监听deadline、取消、进程退出：worker不读stdin、stdout不关、EOF提前/截断、超长帧、stderr无限流、断管均导致失败关闭与有界回收。W1 `read_response`/`eof`当前是同步`Read`，Host应在收到完整有限字节和真实EOF后对内存Cursor调用，不能让主协调器同步等待；worker的同步读取可被Job终止，但外层仍须确认进程和I/O完成。保留对长度上限、逐帧截断、尾随和错误码的独立测试。

## deadline、取消、迟到创建和回收状态机

API以状态而不是单个bool报告：`Preparing -> CreatingSuspended -> Verifying -> Running/Draining -> Reaping -> Complete`；任一步收到timeout/cancel/错误，决策接收门立即永久关闭，转 `Stopping`，向持有者发取消I/O并终止Job，最多2秒内等待真实process handle信号、Job活动数0及全部overlapped完成并释放缓冲；三者满足才可 `Reaped`。响应只在身份/phase合法、输入完全送出并关闭、stdout严格EOF、worker正常退出且Job=0、在deadline内由同一原子提交门获胜时接受；超时/取消先赢或非零退出时即使晚到合法响应也丢弃。用调用代次防迟到回调污染后续请求；不能以任意文本evidence_ref或PID当前不存在解除占位。

准备过程中的文件打开、哈希、ACL和`CreateProcessW/ResumeThread`等同步调用可能在内核或杀毒/文件系统中无限阻塞。建议每个Host实例仅允许一个固定启动线程/槽，绝不为超时调用无限spawn线程；启动线程前先建立由Host拥有且不继承的Job并登记，原子记录取消和调用代次。deadline一到对外可有界返回 `cleanup_unknown` 而非声称进程已退出，保留被占用的启动槽、线程、Job、内存和完成观察权；只在该线程实际返回且其所有潜在process句柄已信号、Job活动数归零、I/O实际完成时，才经内部可信状态转换释放，若无法确认则实例保持隔离不可重用，不能创建新线程绕过。若准备阶段卡在尚未创建Job前，仍占用同一个槽；外层不能说所有底层调用被取消。未知状态须向后续持久Task契约上报，W2组件先独立提供同进程证据；不靠`tokio::timeout(spawn_blocking(...))`假装停止。

迟到创建：创建前、返回后、恢复前均检查同一代次的取消闸；创建操作在截止后才返回成功时保持suspended，不接受stdout/决策，不调用Resume，立刻结束Job并等待进程信号与归零；创建失败但无PID只记真实阶段/错误，不伪造回收事实。`ResumeThread`和取消并发必须明确线性化：在可恢复门内原子授权一次Resume并登记已进入调用；取消先获门则禁止调用，Resume先获门但内核调用卡住时，取消立即关闭决策门并对已有Job请求终止，且直到进程句柄/Job/线程和I/O完成只能是cleanup_unknown。不能仅靠取消标志声称此竞态下绝无一条字节码在截止后执行；若业务要求严格“截止后一条指令不得执行”，该API组合尚无证明，W2不得宣称满足，应由经理裁决实现前置。Create调用超过deadline而尚未返回时，Job关闭/终止与迟到归属的Windows语义也须反例验证，不可假设对未创建进程的Terminate能永久禁止后续进入；保留Job与槽并在调用返回后检查、终止，任何不明结果持续unknown。

### 取消提交门和内存存活的补充约束

将I/O提交视为注册、系统调用进入/返回、完成收割三个独立状态。唯一协调器在共享闸门下先将每个固定数量的OVERLAPPED、关联事件、对应输入/输出buffer、handle和代次登记为in-flight，再授予提交票据；取消原子关闭准入门，之后不再授予任何I/O或Resume票据。一个已获票据的调用可能与取消交错、在取消通知之后才返回或由内核完成，不能声称它一定没发生：登记项和全部资源继续由占槽对象持有，取消侧请求CancelIoEx；如果提交调用当时尚未返回，待其返回后再次对该已登记操作请求取消或收割其同步结果，处理ERROR_NOT_FOUND与正常完成竞态，不据任一次取消API返回释放。每个操作只有在确认同步完成且不会收到额外通知，或收到了属于该OVERLAPPED的实际异步完成（包括ERROR_OPERATION_ABORTED），并完成结果收割后才可移除登记并释放其buffer/事件/handle；多路回调不得拿悬空引用。取消后收到的数据一律丢弃，不能复开提交门。若提交线程本身卡住或完成永远不来，保留最多一个受限槽及其有限内存/句柄，返回cleanup_unknown而不是泄露成无限重复线程或当作成功。

Resume同样先登记进入系统调用的票据；取消获闸门后不再授新票据，但取消前已授票据的调用可能迟到，必须结合Job终止和进程句柄真实信号处理，不虚称物理上绝无迟到指令。TerminateJobObject只针对其调用时已归属的进程；创建尚未返回的潜在进程需在创建线程返回后再次检查并终止，直到进程句柄已信号、Job查询active=0、提交线程无未返回调用、所有overlapped完成收割，才能由内部真实证据释放占槽。Job通知可能丢失，只用于唤醒/诊断，不能单凭通知、TerminateJobObject、CancelIoEx或定时器返回宣告回收。若这些条件无法确认，整个生命周期并未证明有界，只能有限占用并处于unknown。
Host真实死亡：Job只有Host持有，不继承/复制；当Host进程被杀时Windows关闭该进程的Job句柄，预期kill-on-last-handle终止Job中worker。外部观察者仅在测试中提前获得worker**process**句柄，不持有Job句柄；以同步停点区分未创建、已挂起、已恢复及I/O等待，先确认观测句柄有效，再真杀Host并等待worker句柄信号，检查退出码，另核不残留后代/管道，禁止仅按PID不存在推断。验证无其它Host线程重复持有、子进程或其他进程继承Job；Host强杀后不存在Host内状态回写，持久恢复须后续单独契约。前置阶段无PID时应报告未创建停点和控制流证据，不伪造子进程退出。

## 最小代码切片和必要契约

建议新增 `src/bin/wasm_policy_worker.rs` 固定worker入口、`src/wasm_worker_process.rs` Host所有权/创建/状态与有界I/O、`tests/wasm_worker_process.rs` 和 `tests/support/wasm_worker_process/` 中受控卡点/对照worker与外部观察者；`src/lib.rs` 仅导出组件，Cargo.toml/lock仅必要Windows feature与明确bin。测试恶意worker必须通过**仅测试编译**注入工件，生产解析/manifest/环境绝无可指定worker路径；测试helper必须以可审计方式隔离，不能因测试通道给生产任意native启动。复用W1 protocol/executor，不改变其语义或原六项；若新增异步传输需要只读内存解析/完整响应校验API，必须另列最小共享契约、唯一写入人并维持帧兼容，不能绕过原验证。工件发行路径与hash注入、同用户token/系统库策略、状态与cleanup_unknown占槽、错误分类、测试停点和经理最终整合均需正式任务单冻结；此处不授权产品实现。

## 可以独立证伪的矩阵

1. 正向：真实WAT模块在8MiB上限边界/近边界输入、完整三帧与EOF下执行，完整manifest/nonce/generation/phase匹配，正确结果只提交一次；低于响应上限输出、正常退出和Job归零。既有W1六项再由受控组件窗口保留，不能将其静态单元测试说成真实进程成功。
2. 绑定/工件：改变nonce、generation、digest、plugin版本或阶段；重放旧响应、额外帧、空帧、截断、无EOF；映像重解析、硬链接、创建期间替换、预持写句柄/映射及错误哈希；均应明确失败且无决策提交，无法固定工件即不resume。
3. 管道：worker永不读stdin（超过管道容量的真实8MiB写）、只读部分/提早关闭、从不关闭stdout、帧后额外字节、无限stderr、stdout无限/超限、两路同时压满；测试调用deadline、取消、底层I/O完成及真实process/Job回收，不以线程被丢弃作为完成。
4. 计算与竞态：真实WAT的编译/实例化/start/run不同耗尽或死循环，内存/CPU限制；在映像检查、管道连接、Create前/内/后、挂起、Resume前/内/后、写入中、读取中注入取消和失败。记录线性化结果、迟到返回、禁止旧决策；人工卡住的同步调用须证明线程槽上限及unknown不可重用，而非把模拟停点充当OS调用必然能取消。
5. Host死亡：外部观察者持真实worker进程句柄，真实杀Host于suspended、resume后、stdin阻塞及输出阻塞；Job只在Host内，验证worker信号、无后代/管道句柄残留；未创建事件另记。检查Host内无重复Job句柄及worker无法继承；失败时原始事件和反例保留。

尚需实证的关键点：同用户native worker的安全授权与系统DLL目录、快照/映像及已存在映射TOCTOU、目标系统原子Job与overlapped管道完成/取消、最坏阻塞的清理语义、Host死亡窗口、资源上限，以及W1配置在128MiB下的实际可用性。单项正向可独立证明真实S01和8MiB传输；任何一项通过都不关闭A/P08或直接允许生产WASM接入。

