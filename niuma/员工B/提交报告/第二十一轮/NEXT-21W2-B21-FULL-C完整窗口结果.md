# B21-W2-FULL-C 完整受控窗口结果

2026-11-02，员工 B。经理签唯一窗口，受测 `f898f0b372bada37e2b962890c3dca23935c2f40`、产品 `77d0ecdf81b4ad90029bbc85db06271cc4478276`、Rust `43a9b604fb5e21b111cc831d98d2a92794d0962f`；源码前、构建后、最终状态同三项、工作树/index 空。窗口 UTC `2026-11-01T16:46:09.6110358Z`–`2026-11-01T16:49:14.2099232Z`。原现场 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-c-f898-attempt1/`；E 输出、本人独立旧 target/TMP/TEMP、offline/locked、jobs2、debug0、incremental0；实际命令/完整路径、MSVC/SDK 在现场 `start-window.ps1` 和 `environment.json`。外层隐藏 PowerShell 实测 MainWindowHandle=0；外层 DirectProcess flags `0x08000004`，W05 同编译 helper `CREATE_NO_WINDOW`；没有对所有瞬时窗口进行完整可见性监测。

Add-Type 成功；worker/probe build 和 lib no-run 的 `build-meta.json`、`test-build-meta.json` 均实际 exit0；`selection.json` 记录 full/21，list 原对象 exit0，完整21+3内部helper映射核对通过。以下21个 exact 按顺序各在新现场运行，原 test 进程 PID/creation FILETIME、UTC、exit 和日志路径在各 `case.json` 以及由原件生成的 `case-summary.json`；内部 helper 不计入21。每例均 exit0、libtest 1 passed/0 failed，窗口 exit0。

| 序 | exact 后缀或项目 | test父PID | exit |
|---:|---|---:|---:|
|1|w01_real_worker_before_after_and_near_8mib|78580|0|
|2|w02_protocol_fault_matrix|77508|0|
|3|w03_pipe_fault_matrix|75600|0|
|4|w04_resource_limits|74024|0|
|5|w04_cpu_budget_boundaries|78160|0|
|6|w04_injected_zero_read_sensitivity|39776|0|
|7|w04_unknown_cleanup_and_slot_reuse|75056|0|
|8|w04_cancellation_and_acceptance_boundaries|9568|0|
|9|w04_injected_membership_failure_reaps|21192|0|
|10|w04_persistent_observation_errors_retain_io_and_slot|39444|0|
|11|w05_host_kill_boundaries|78612|0|
|12|w06_p01_live_job_observation|45524|0|
|13|pin_file_counterexamples|56460|0|
|14|pin_existing_writable_mapping|77048|0|
|15|process_image_identity_counterexample|77724|0|
|16|sentinel_explicit_inheritance_control|18360|0|
|17|w06_non_whitelisted_event|77728|0|
|18|w06_job_handle_not_inherited|58420|0|
|19|active_limit_single_factor|65732|0|
|20|breakaway_single_factor|74564|0|
|21|suspended_process_in_real_job_requires_process_fallback|71492|0|

W01 before/after 真实 worker PID41480/78168，各 exit0、signal、Job0、IO完成，before 模块8388544字节。W02/W03 exact 本现场通过，不扩写为所有协议/系统环境。W04 resource job-cpu PID48192：测试专用 Job CPU 限额已查询为300000000；真实原进程首次采样0，随后 user=30781250 个100ns、wait=258、Host Gate 赢，返回 `worker_cpu_budget`，Stopper/原句柄/Job/IO 收束；elapsed 941ms。该 fixture 提高 Job 限额是防遮蔽，不能当生产 Job CPU 限额实测；低CPU真实 W1 正控另核生产 Job 30000000。采样和 Gate 间不是原子OS动作、不声称严格3秒硬实时。

case-05 查询失败、取消/截止先胜及最终接受门均通过该例断言。最终有效响应夹具 PID78372 的真实进程 user=30625000、原对象 wait0、协议响应验证成功，记录 `cpu_budget_final_reject`；另取消/截止先胜夹具 PID68800/77100 分别记录真实 user=30625000/30781250，不冒称 Host 主动结束。CPU Job 提高和跳过周期采样均为测试态标记。无效句柄查询使用真实 API 失败，不伪造 CPU 数据或 Job 通知。

case-07 W04 真实 W1 正控 PID54552 通过；真实 CreateProcess 不兼容 flags 测试返回 error87、实际 flags525340、Job active0/total0；正常 `create_after_ticket` 本现场返回 error5、flags525324、Job active0/total1，经 Stopper join/协调器返回和 observer 解除检查。**该正常停点本次未实际走 late-success**；另外 `create_returned_suspended` 实际创建 PID57112 并凭原句柄 signal/Job0 回收、未 resume。不能用本场 error5 反写旧249479的未知错误或把另一个停点当成本场 create_after_ticket late-success。

W05 四停点实际 TerminateProcess 均返回1，before/suspended/resumed/pending 的返回时 elapsed 分别435/778/336/450ms，均小于3500ms；before 无 worker 对象断言，其余原 worker 对象 wait0。父方无额外 Job 持有，详情 `case-11/test.stdout` 与四个独立阶段文件。W06 P01 本例 PID75300 正常收束并符合该例观测边界；工件反例、实际进程映像、匿名事件正负控及 Job 不继承的 exact 在真实前置成立条件下通过，stderr/stdout 没有 not established；映射例记录写映射被拒错误32，实际映像 PID59500、正控事件 helper PID5672。单因素 active2 正控真实挂起子进程 PID61708 成员1、定向 terminate1/wait0；active1 失败 error1816。breakaway positive PID78036 非成员、挂起且 terminate1/wait0；固定 active2 下 no-breakaway 失败 error5。只能按具体夹具场景解释，不能把错误码单独当普遍归因。

三件源与固定副本前后 SHA256：worker `05d1a4b1c8c089f175ec316001036723930ba8dbdd4e7fb26ffca415f0770691`；probe `6691e01d920e457d942d5a3e17883ea55e35a0dbbeb056c10d139bc530c70e6a`；lib-test `6b98bbaa4313d0d5e72b6ed13c7847858e2a41a6f7262ad62e86d6dd560ca38d`。runner逐例前后校验，窗口结束后再核21份 worker 和1176份 probe 副本，0差异，见 `runner/hashes-after-window.json`。仅本次固定件对应本次提交，不借历史构建。原 stdout/stderr 搜索未见 `not established`、`NHC_TERMINAL_UNKNOWN` 或失败结果。

结果交经理核证；结束时 gate absent、独立树 clean/index 空、.NET 快照无匹配重型进程。未执行 fmt、Clippy、受影响回归、非Windows目标或主线整合；完整W2/项目验收由经理裁定。旧 FULL-B Deadline 和249479未知失败的历史事实保留，不由本次结果反写。
## 2026-11-02 原始证据发布

已将本窗口 218 份必要日志、JSON、脚本及阶段文件逐字节发布到本报告旁 `B21-FULL-C原始证据/`，共 814626 字节；排除 EXE、target 和 tmp。`双hash清单.json` 保存源相对路径与源/发布 SHA256，清单 SHA256 `18eadb548258a5096c878d1159eb94cd49252a9ec3294602b90895d159c549e6`；`发布复核.json` 记录0差异。原始日志中的绝对路径与空白未改。等待经理独立核证。
