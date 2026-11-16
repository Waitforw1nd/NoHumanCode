# D：W3 可信 bootstrap 与同域接线候选

基线 `1d09a551`；仅本人新树的指定源码和本人档案。main 唯一编译期读取 `NHC_PACKAGED_WORKER_SHA256`；缺摘要禁用 worker，严格 64 位小写 hex，配置错误/文件缺失或哈希错误失败。固定解析 Host 可执行文件同目录 `peachsh-wasm-worker.exe`，Windows 使用既有防重解析、单链接、禁止写及删除共享的 image pin；service 保留文件和父目录句柄，启动时既有检查仍比较进程实际 identity 和 hash，协议在实际调用的严格编码/响应解析处验证。`--check` 同样执行 bootstrap，未改变 instance lock 与 runtime teardown。

`WorkerService::bind_store` 绑定唯一 Store；生产调用必须取得 task-local 原 operation 并通过 A 的 `Store::execution_generation` 验证同域及 admission。平台线程与结果侧分别持 operation body，generation 在平台真实返回及结果侧完成后才 settle；cleanup unknown 同步 seal，迟到完成不重开域。W2 的 process/Job/I/O 监督逻辑保留。B 负责 Engine 安装并调用 bind_store；A 负责模块注册和 generation API，main 调用 B 的 `install_packaged_worker`。公开 run_wasm 继续拒绝，非完整 W3。

验证：单文件 rustfmt 通过，`git diff --check` 通过；新增纯 mock 检查双完成次序和 unknown、摘要格式测试，未运行。按任务禁止运行 Cargo、真实进程、探针及安装；真实构建、pair、联合编译、生命周期测试均由经理执行。此提交依赖 A/B 接口合入，单独基线不可编译；不得将本报告视为完整生产验证。
