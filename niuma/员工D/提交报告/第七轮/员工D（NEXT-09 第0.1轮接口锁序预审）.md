# D-R7-01 第0.1轮接口与锁序预审

2026-09-27，员工D GPT-6-astra，执行ID D-R7-01-20260927-E01，经理新轮正式直接派发。审查树 ../nhc-d-restore-review，分支codex/d/restore-review，HEAD4767f53971eedd73a8216b1c68801e6cdce8bd79；NoManCode tree9800ad0ebcabf556c517cade04eaa8b433057ab6，Rust treeca006f88165d346ee6e60910079e9dece53aff25，真实status空。已完整读NEXT-09契约1.0与本人第七轮任务包。全部项目操作E盘，无源码写入、无测试执行；第六轮冻结，不复用旧门禁当本轮结果。

## 预审结论

冻结接口在原锁序及编译结构上可行，目前无阻断实现的接口冲突；固定候选R06仍待经理派发，不能预写通过。

store.rs:1103 finish_restore_path获取DB mutex后开事务，UPDATE outcome/change后tx.commit，方法返回会释放连接guard。Engine必须在成功返回后才发OutcomeCommitted，不能把回调移进Store内部或commit前。两restore路由均持Engine gate进入restore_locked，此排他必须保留；回调可用独立SQLite连接读取刚提交事实，不调用任何需要同Engine gate的异步方法等待，否则自锁。probe mutex只用于匹配Task并clone Arc，调用前释放；跨Engine/Task隔离、clear后行为仍需实测。

src/restore_http_tests.rs作为lib的cfg(all(test,windows))模块可访问crate::engine的pub(crate)hook，也可使用pub server::router建立真实axum listener，用reqwest发HTTP。真实链应是HTTP client→loopback server→生产router/handler→同一个装hook的Engine→Store→FS，不以router oneshot或直接Engine调用冒称真网络HTTP。普通integration test的库依赖没有cfg(test)，因此旧tests/workspace_http.rs不能直接复用新hook；迁入内部test模块解决的是编译可见性，不降低真实HTTP覆盖。

同步Fn回调不能阻塞唯一runtime线程；需多线程runtime或独立阻塞线程配合有限期双向通道。ready/continue超时均需释放等待，异常与断言路径必须有清理guard；不能只用异步timeout包住在同线程被同步回调卡住的future。callback不应持DB锁，亦不应通过Engine gate内方法读取outcome。固定候选检查超时后仍在运行的HTTP请求是否被可靠收尾。

所有probe类型、Engine字段/方法、初始化与调用必须cfg(test)，C模块cfg(all(test,windows))；不能只gate模块声明而留下正常构建对类型的引用，不用debug_assertions。普通cargo check/bin/wasm不应编译probe；cargo test时lib本来有该模块，这是所需验证环境。无新增feature/依赖/生产端点。

错误码应从原anyhow链找io::Error.raw_os_error、rusqlite::Error::SqliteFailure的extended_code，缺码即None，不能猜码或保存to_string。Event带task/path/stage可在私有测试内使用，不加入HTTP DTO、事件数据库或公开日志。阶段必须在实际失败操作处标记，不把所有恢复失败统称FileAction；OutcomePersist须在错误事务已回滚/返回后通知。原Unknown/Partial与receipt完成计数保持。

## 提前提示B/C

C在A OutcomeCommitted之后对B打开share_mode(0)，按现有engine.rs:2038–2058流程，B先root/resolve再read_safe_file，然后才atomic_replace。排他B句柄通常先使ReadCurrent失败，未必进入FileAction。R02可证明A完成、B失败、C未尝试，但不能强行把该错误标为FileAction。R03须另用真实受控方式到达FileAction并取得OS码（例如在ReadCurrent完成后控制不共享删除的读句柄，或明确FileAction前阶段停点），不修改正常生产流程以满足标签。已报告经理，等待其统一通知实现者。

本片任何受控错误及10次重复通过只能证明新机制，不等于860历史根因。下一第一操作：收经理指定固定B/C候选，再核真实源码、回调锁序、编译隔离、HTTP事实和异常清理；不自行读变化态作最终证据，不运行正式门禁。