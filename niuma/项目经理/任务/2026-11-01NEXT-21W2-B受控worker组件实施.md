# NEXT-21W2 B：受控worker组件实施

2026-11-01，项目经理。原B第二十一轮继续实现，基于本人设计及经理《W2设计裁决》；A树冻结，不继续原生profile试验。核心优先，完整项目范围不缩减。

## 基线、所有权和Git

经理已建独立树../nhc-b-wasm-worker-process，分支codex/b/wasm-worker-process，基线7dd3728f58985244dbe09f5486e4568e94bde408，产品7267b1187c5e46f6632cf546bc6ab8638d33a3b9、Rustfb9828e63c4f11c5633c83e570b761de6dfc438d。B核三SHA/clean，唯一该树产品提交者；经理唯一主仓Git与整合。C在另一树修W1-ID01并占动态窗口；B当前静态/fmt/diff-only，固定后一次整体静审再串行验证。报告/身份只写主仓niuma/员工B/提交报告/第二十一轮/及本人身份。

允许产品路径（相对NoManCode/rust-app）：新src/wasm_worker_process.rs、新src/wasm_worker_process_tests.rs（cfg(all(test,windows))内部测试可访问crate-private可信工件构造）、必要tests/support/wasm_worker_process/；examples/wasm_worker_process_probe.rs仅受控故障/Host停点probe；src/lib.rs仅模块注册；Cargo.toml仅必要Windows API feature，Cargo.lock仅这些feature确有必要时。复用现有peachsh-wasm-worker，不增第二worker入口。不改W1 protocol/executor/bin、Engine/Store/schema/Agent/PluginHost/server/CLI或旧测试；确需额外路径先列具体原因。不要整段复制A封装后声称已验证，原A尚未通过；可复用已读算法，但新路径单独实测。

## 组件契约

- 提供受信Host持有的有限并发服务（初始1槽，Busy可观察），纯策略调用异步可取消。服务取得固定可信worker工件，普通调用仅交W1模块快照/纯DTO/绑定身份，不能交可执行路径/命令/环境。随机request/nonce的生成与expected身份必须明确由Host负责，不凭worker回显证明可信。安装描述符内部构造、测试替换仅cfg(test)，不接可写旁文件自证。最终build后独立固定工件hash，运行前后核映像身份/摘要；拒绝reparse/替换/预持写句柄等，列真实加载例外和威胁假设。
- 复用现有W1三帧及完整响应校验，Host只处理有界字节/摘要/DTO，不解析不可信模块。正常运行固定当前用户受信wasmi worker，无WASI/imports；不宣称Job提供文件/网络隔离，不接受任意native代码。仅显式最小环境/工作目录，句柄白名单stdin/out/err；本机命名管道首实例/远端拒绝/最小DACL，实际验证连接端归属和抢占反例。worker不接收管道名。
- 创建时Job-list原子归属，Job不可继承且只在Host进程持有，active process1、no breakaway、kill-on-last-close；挂起核实际Job/映像/资源限额再resume。初始128MiB/3秒CPU/5秒调用/2秒回收；绝对deadline从准备开始，成功提交须在截止前，不能逐步重置。
- 三路overlapped I/O并行，每路有限未完成操作/缓冲，8MiB请求流式发送完成后真实关闭stdin；响应限长+完整帧+EOF，stderr最多64KiB且产品诊断仅固定类别/count/摘要。OVERLAPPED/buffer/handle存活至同步完成或真实异步完成收割，CancelIoEx/ERROR_NOT_FOUND不是释放证明；禁止隐藏同步write_all堵住整个5秒边界。结果Outcome::Error不等于策略成功。
- 保持单实例有界启动/协调资源，票据先登记再提交，取消关门后不授新票据；迟到Create成功仍挂起并终止，已授权Resume竞态不虚称零指令。调用方有界返回后未确认者保持cleanup_unknown及有限占槽，不能drop client/重建service绕过，也不无限spawn新线程。caller future Drop请求取消且不释放在用OS资源；后续只有真实process句柄信号、Job活动数0、创建/提交系统调用已返回、全部I/O完成才能可信释放。同步系统调用可能卡住的事实明示，不能把spawn_blocking超时当底层终止。
- 任意启动/观察/解析失败关闭，不降级普通native，不采纳迟到或重复响应；成功须输入完整发送/关闭、完整manifest/nonce/generation/phase匹配、stdout EOF、正常进程退出及Job空、deadline接受门赢。无需改DB/恢复表；持久unknown/Task lease后续单独接线，本轮给真实组件状态与证据。

## 验收矩阵

W01真实固定W1 worker运行有非平凡start、before/after策略的WAT，包含接近8MiB有效模块（三帧总量超过管道容量），输出由真实模块计算，确认正常退出和Job空；不是fakeworker回显。
W02协议反例：截断/超限/额外帧/不EOF、错nonce/generation/digest/version/phase、非零退出、合法响应迟到或重复，均无采纳。
W03管道：真实超过容量的stdin无人读取、部分读关闭、stdout不关/超限、stderr无限、双路压满，期限/取消和I/O完成证据共同核；故障probe仅测试替换入口。
W04资源与取消：真实WAT start/run循环、内存超限，含worker CPU/内存限额；各启动/submit/resume交界取消/故障，人工卡住系统调用点明确是注入而非证明OS必返，验证unknown占槽及后续真实释放，无重复worker。
W05真实Host强杀：外部观察者仅持worker process句柄，分别在挂起、resume、I/O阻塞停点杀Host，等待worker句柄信号；无额外Job持有者、无后代。未创建交界用真实控制事件，不伪造PID。stop握手/观察者自身失败也有界回收。
W06工件/句柄：错hash、reparse、路径/映像替换、预持写句柄等反例；实际非白名单句柄不可继承，Job数量/不breakaway；非Windows显式unsupported并保持编译边界。

## 验证和报告

先完整静态候选、三SHA/差异与矩阵实现映射，一次整体审后唯一动态窗口。建议窗口为check，显式构建worker及故障probe，先固定并复制工件核hash，再按W01→W02/03→W04→W05→W06各exact单线程，成功后fmt/Clippy；不在测试内递归cargo。可先有限基础可行性窗口但完整矩阵不取消。普通授权内编译/lint/夹具修正新commit/newattempt可继续；权限/范围/断言变化须经理裁决。

E盘工程/target/tmp/log，offline/locked，复用已保全日志之外target减少重复编译。原件与来源清单保全，首失败停；构建artifact在所有可能改写target的步骤之后再固定，不以旧共享target路径声称原件不变。报告逐项通过/失败/未执行及unknown，源码与实际worker/Host/probe身份、异常回收限制必须可复核。W2未接生产Agent/PluginHost，完整核心和项目仍active。
