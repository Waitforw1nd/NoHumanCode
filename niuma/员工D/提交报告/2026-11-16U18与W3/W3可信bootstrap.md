# D：W3 可信 bootstrap 与同域接线候选

基线 `1d09a551`；仅本人新树的指定源码和本人档案。main 唯一编译期读取 `NHC_PACKAGED_WORKER_SHA256`；缺摘要禁用 worker，严格 64 位小写 hex，配置错误/文件缺失或哈希错误失败。固定解析 Host 可执行文件同目录 `peachsh-wasm-worker.exe`，Windows 使用既有防重解析、单链接、禁止写及删除共享的 image pin；service 保留文件和父目录句柄，启动时既有检查仍比较进程实际 identity 和 hash，协议在实际调用的严格编码/响应解析处验证。`--check` 同样执行 bootstrap，未改变 instance lock 与 runtime teardown。

`WorkerService::bind_store` 绑定唯一 Store；生产调用必须取得 task-local 原 operation 并通过 A 的 `Store::execution_generation` 验证同域及 admission。平台线程与结果侧分别持 operation body，generation 在平台真实返回及结果侧完成后才 settle；cleanup unknown 同步 seal，迟到完成不重开域。W2 的 process/Job/I/O 监督逻辑保留。B 负责 Engine 安装并调用 bind_store；A 负责模块注册和 generation API，main 调用 B 的 `install_packaged_worker`。公开 run_wasm 继续拒绝，非完整 W3。

验证：单文件 rustfmt 通过，`git diff --check` 通过；新增纯 mock 检查双完成次序和 unknown、摘要格式测试，未运行。按任务禁止运行 Cargo、真实进程、探针及安装；真实构建、pair、联合编译、生命周期测试均由经理执行。此提交依赖 A/B 接口合入，单独基线不可编译；不得将本报告视为完整生产验证。

E 审查澄清：结果侧完成指 `WorkerService::call` 内的 gate.accept 与返回，不覆盖调用方后续持久采纳；目前无生产调用链且公开 run_wasm 拒绝。未来调用方必须在原 operation 范围持票，并在持久采纳事务中使用 A 的 fence 和 commit 封门；需要独立验证取消、恢复与 unknown。future 被丢弃时结果侧结束，平台线程仍持原 operation/generation 至实际收束；不能据此声称完整 W3。

## 静态自审与 D→E 交叉审

自审发现监督线程 unwind 时原实现可能在等待方消失后丢弃域票并释放静态槽；已加线程 RAII，异常时同步 seal 原 generation、保留槽和持票对象，不猜测进程/Job/I/O 已回收。正常 spawn 失败在结果侧退出时结算；正常平台返回与结果侧结束均完成后才 settle；接收通道断开优先判 unknown，不被取消分支遮盖。future drop 停 gate、结束结果侧，线程仍持原票直至平台结束；平台异常时保留原持有。此为静态分析，尚未执行测试。

对 E 已提交 `26ada6ff` 及其当时未提交的监督修订做只读初审：缺摘要、格式、文件缺失、错 hash、混搭、存活 pin 和释放后替换有对应夹具；真实 pair 的失败原因检查避免把任意启动失败当通过。构建从冻结 worker 工件计算摘要并注入 Host，pair.json 不作为认证来源，记录明确不证明完整可复现环境。E 的测试子进程监督只证明直接 child，后代回收仍依赖外层 lifecycle 窗口，不据此声称通过；最终 E SHA 与真实执行待经理核验。

D→E 固定复核 `75b06911`：子夹具确认实际运行一项，混搭读取独立 worker 并记录输入路径与 SHA256；外层复用 lifecycle gate，unknown 标记停止后续执行，直接进程退出不冒称后代收束。两项待修：C 所有 `tests/support/owned_child.rs` 的 output 先接受已退出状态，首次观察若已过 deadline 仍可误判按时完成；E 的外层 exact ignored 测试仅凭 Cargo 退出码，零项运行也可能通过，应核真实一项通过。已分别通知 C、E；未动态执行，结论待其返工固定后复核。
