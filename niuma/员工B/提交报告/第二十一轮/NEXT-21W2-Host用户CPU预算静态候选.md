# NEXT-21W2 Host 用户 CPU 预算静态候选

2026-11-02，员工 B。经理正式裁决原 W2 范围静态实施；C 持动态窗口，本轮无编译或动态。基线 `82a0016ef1d582abc963270b48149e10fca1220f`、产品 `99ed0add4a89fbee1473a3c511a9972a54148a65`、Rust `8fccff04244c3a3b04e120f7bbb2b6e3d5ecdea4`。新提交 `3e5ac1bd1db9b32d43f15f0bcafdd95de93684e9`，产品 tree `01ad4dd9e1a7f6f247a57e2c873f957e8b0a990b`、Rust tree `ee180ebc2d31e6d32ea516b771ad730dff691e13`；独立树 clean/index 空。

## 原锁与证据

经理核定原父对象 exit101、14次 ProcessGuard signal/Job0/三路 IO 结算和当前无活动重型对象；按授权将 `nhc-w2-window.lock` 原样移动到 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-b-82a0016e-attempt1-audit/case04-gate.original.json`，前后 SHA256 均 `a73a9675dff8b8009a83ee07d90d5902519c9c97c46069c6bc6e9ec964ce59e6`，依据和 incident hash 记同目录 `gate-archive.json`。未删除现场、未猜杀。FULL-B 现场与 audit 的97份必要日志、JSON、脚本逐字节发布到本报告旁 `B21-FULL-B原始证据/`，排除 EXE/target/tmp；`双hash清单.json` 包含源相对路径及源/副 SHA256，清单 SHA256 `c640448d222c1ebc735bd31d9b6cac292ddf5677cba49a749df3cf1701d4a8f9`，`发布复核.json` 为0差异；原始绝对路径和空白未重写。

## 实现与错误语义

`src/wasm_worker_process.rs` 的 `CpuBudget` 使用真实 `ProcessGuard.process` 创建句柄调用 GetProcessTimes，拼接用户态100ns累计值，不按线程或核数折算。恢复后首次采样，现有 I/O 协调循环约10ms限频，process 已 signal/Job0 且准备返回完整响应时强制最终采样。阈值固定30000000；原生产 Job PROCESS_TIME 同为30000000、5秒绝对 deadline、2秒 cleanup wait、unknown 占槽及现有资源收束保持。无新生产线程、公开参数或环境开关。

跨线或查询失败先捕获真实 Win32 错误，再在 Gate 锁内检查实际 cancel/deadline 并封门；赢得仲裁分别返回现有类型的 `IsolationUnavailable("worker_cpu_budget", None)` 或 `IsolationUnavailable("worker_cpu_query", actual_code)`，优先已发生的取消/截止。实际 caller 仍按原 biased select 与清理完成时间裁决；若清理在截止前未完成，不伪称 CPU 已先赢 caller。经原 Stopper、Operation、ProcessGuard 和 slot 收束；未确认仍保持 cleanup_unknown。正常 exit0 的已完成响应在最终采样>=3秒时被拒绝，不能送 Gate.accept。

为区分 OS 与 Host：跨线时原进程句柄若已 signal，周期采样不声称 Host 主动终止；最终采样发现已 signal 且 exit非零，保留原 worker_exit 路径，不合成 Job CPU 事件。若采样观察进程仍活且 Gate 的 CPU 仲裁成功，测试态记 `cpu_budget_host_won` 和实际 wait=258；两者之间仍有不可原子化的退出竞态，不单凭此声称 OS 一定未并发退出。已正常退出且 final 跨线记 `cpu_budget_final_reject`，不是 Host 杀死进程。GetProcessTimes/进程 wait/exit 查询失败即时捕获错误并 fail closed，不伪造 CPU 读数、进程退出或 Job 通知。该方案按协调循环采样，调度延迟或系统调用阻塞可能超出3秒才观察到，并非硬实时上限。

## 测试映射与注入边界

- W01 两次低CPU真实 W1 worker 正常响应、原 process/IO 及真实 Job 配置 30000000 均断言；`w04_cpu_budget_boundaries` 另有同样低CPU真实 W1 正控。
- 既有 `w04_resource_limits` 的 job-cpu 使用真实8线程忙 probe 与同一生产 launcher/原句柄，test-only 私有 hook 仅该夹具将该 Job CPU 限额提高到300000000，并查询确认实际值；其余 Job/IO/5秒期限不变。要求真实原对象用户 CPU 跨30000000、采样时 wait=258、Host Gate 赢、返回 `worker_cpu_budget`、三路收割/原句柄 signal/Job0。Deadline 或 OS先退出均不算 Host 预算通过；若环境不能在5秒内形成3秒用户CPU，则该正例不成立。其他 W04 真实资源例与生产 Job 配置不放宽。
- 新 ignored exact `wasm_worker_process_tests::fault_matrix::w04_cpu_budget_boundaries`：使用真实 W1 的低CPU正控；test-only 向真实 GetProcessTimes 传无效句柄，要求实际查询失败错误6并封门、原真实 worker 清理；在首次采样停点让取消/截止分别先于该真实失败，再核返回类别；`cpu_final_checked` 核已采样后取消不能接受。
- 三个 `cpu-valid*` 独立 probe 副本以真实本进程 GetProcessTimes 及8忙线程累计到30500000，再产生真实协议响应。test-only 专用 hook 明确记录跳过周期采样、Job CPU 限额提高及查询结果；测试态对接收的响应执行协议/身份/策略校验并记 `cpu_fixture_response_validated`，随后最终采样拒绝有效超预算响应。另在最终采样前分别让真实取消或截止先胜，核真实 CPU 跨线而不误报 Host 预算。此跳过周期采样只为单因素验证最终门，生产从不跳过；如前置CPU跨线/响应/Job配置不能成立，测试失败或未覆盖，不能把 Deadline 当成功。
- 原 W04 取消后真实 CreateProcess 失败正控和 late-success 证据仍保留；W05/W06 未改变。runner 默认映射增至21 exact + 3 helper，仍逐例全新现场/串行/首败停窗；`-ExactTest` 只接受当前映射中的一个名称。

本次修改5文件：`src/wasm_worker_process.rs`、`src/wasm_worker_process_tests.rs`、`examples/wasm_worker_process_probe.rs`、`tests/support/wasm_worker_process/prepare-probes.ps1` 和 `run-controlled.ps1`。三个 Rust 文件 rustfmt/check、两个 PowerShell 文件 parser、diff-check 通过；静态映射数为21+3。未运行编译、测试、runner、worker/probe/helper、Clippy 或非 Windows 目标；所有跨线、错误注入、结果竞争、OS先退出和 21 项矩阵均待经理集中 review 后另签动态窗口。原 FULL-B Deadline 失败及其未执行范围保留，不能按本候选反写旧根因。

## 2026-11-02 状态更正

上文首段“C 持动态窗口”是过时表述；经理确认当前 B、C 均仅静态，尚无新的动态授权。上文其他历史观察不变。经理已独立核对 FULL-B 97 件原/副本，428872 字节、双 hash 零差异。
