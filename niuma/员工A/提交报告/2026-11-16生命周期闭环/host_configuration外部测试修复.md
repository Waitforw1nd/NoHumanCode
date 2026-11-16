# host_configuration 外部测试关闭与恢复顺序修复

2026-11-16，员工A。经理依据本轮用户授权将 `tests/host_configuration.rs` 与 `tests/host_configuration_security.rs` 唯一写权转交A，C已排除。仅修改这两个外部测试及本人报告；生产组件、probe、Cargo依赖、runner与集成树未改。

## 固定失败依据

只读核对 integration 的 `.local/lifecycle/integrated-external-1/01-external-tests.log` 与 `source-before.json`：受测HEAD `4a02267579aa44b0e9fb579506818455086937aa`，产品tree `228ef5760bfce76550f188ff42fdd0ac39f024fe`，Rust tree `3b4c52d884de0c56bd02635030e22a4fad1aa789`。原两文件在该树与基线相同。

|原失败|日志事实|本次调整|
|---|---|---|
|real_engine_busy_two_projects_old_task_and_unknown_budget_are_preserved|host_configuration.rs:233，活controller下recover拒绝；该target 17过/1失败|abort并join原pending后，先断言活controller恢复失败且全部持久行不变；shutdown并join原Engine后台任务、drop旧Engine，才由原Store recover并创建新Engine。末尾降级schema前也shutdown/join/drop|
|configuration_preserves_nonempty_unknown_budget_old_task_and_grants|host_configuration_security.rs:1245，同样活controller恢复拒绝|保留原abort+join边界；先验证拒绝与持久行不变，再完整停止旧Host，重开同一原库，沿Host::start已有recover→Engine::new路径恢复。新Host token追加到原forbidden集合|
|i08_lookup_404_during_received_put_does_not_resolve_unknown|scan_persisted:121，Windows错误33，文件区域仍被锁定|Relay abort后await真实任务退出；Host由graceful shutdown等待所有连接任务结束，再Engine shutdown/join/drop，验证Engine和Store已无强引用后进入原scanner|
|i08_real_commits_then_hostile_responses_are_unknown_without_retries|scan_persisted:121，同样锁文件读取失败；security target共4过/3失败|同上，覆盖所有原hostile变体和两种输出模式，无跳过分支|

原分配 `checkpoint.rs`、`workspace_changes.rs`、`workspace_crash.rs` 在external-1均通过；经理要求不做无谓修改，三个文件没有落改或进入本提交。

## 保留的验证边界

- `scan_persisted` 函数正文保持逐字不变：Host释放后先读取原目录所有普通文件，再打开原DB只读连接取DPAPI封装指纹；保留原DB、存在的WAL/SHM、instance.lock与日志的全字节扫描。没有排除锁文件、吞掉文件读取错误、复制换库、删文件或删敏感数据扫描。
- Host正常stop等待Axum graceful shutdown返回，随后要求Engine shutdown报告settled；最终drop自身并断言Engine/Store弱引用均无法升级。超时、server panic、非settled或残留owner均失败，不将abort请求当作持有者已释放。Drop中的abort仅保留异常退出兜底。
- 两个unknown预算夹具仍从真实已到达provider的pending请求abort+join生成未完成持久调用，再经真正恢复归类unknown；保留unresolved_count=1、held_tokens=100、spent_tokens=5、真实调用计数、旧Task原值、两项目授权、元数据更新/凭据轮换和schema迁移断言。不把业务持久unknown改判为成功，不放开生产单owner规则。
- I08仍保留真实Host commit、CLI未知结果、404不解决在途不确定性、同key显式重放、原回执、零自动重试/零provider调用、完整输出扫描。重启新增token也进入敏感集合。
- HTTP/Relay与Host.stop均为测试私有接口，无生产API或外部JSON契约变化。

## 静态验证与未验证项

仅对两个文件运行rustfmt（edition2024、skip_children），并执行git diff --check，均通过。逐段检查删除行：仅替换旧abort-only停止、两处drop(relay)及活Host直接recover；原业务断言保留，新增关闭与拒绝断言。

A未执行Cargo、编译、clippy、测试、CLI、Host、probe或其他真实测试进程。新关闭路径的实际连接收束、弱引用归零、预算unknown持久恢复以及22个hostile子场景须由经理在下一个固定集成窗口验证；原external-1失败不提前改判。建议定向运行上述四例，并覆盖两个target全部18+7例，因为Host.stop为security七例的共享夹具。

经理当前component-final-build-1/25例矩阵使用冻结集成输入。本次只改两个外部fixture，不更改组件、Probe、Cargo依赖或runner；不据此自行宣称组件25例通过。旧case16 unknown继续保持。
