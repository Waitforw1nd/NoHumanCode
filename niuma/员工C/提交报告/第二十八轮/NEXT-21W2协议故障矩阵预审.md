# NEXT-21W2：协议与管道故障矩阵预审

2026-11-01 · 员工 C / 6.1 Terra。固定 `c63d7da2f316eb13f7150202d49f6c960aeb62df` / 产品 `3ad8244e9071707c38460c7eeaca44f422245fd3` / Rust `6521dd44ce2a22bda71e18e5e325030a614f5b60` 的 W1/W2 Git blob，只读审查；W01 真实 worker 已通过，下面 W02/W03 均为待实施/待执行，不把 W01 升级为 W2 全部验收。本报告另按经理指示单独查看 `8937aed1` 的零写 probe/test 增量，不把其在途 HEAD 当作本文固定基线。

## 已知代码事实

- 固定 W2 `wasm_worker_process.rs:1030-1098` 对 stdout/stderr `Ok(Some(0))` 直接置 EOF。Microsoft [ReadFile，Pipes 段](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-readfile) 的正文说明 pipe 成功读到 0 字节可由对端 `WriteFile` 零长度造成；不能据此证明 pipe 关闭。本机 byte-mode named pipe 能否真正观察零读尚未动态确定。若合法帧后出现零读、再跟随垃圾，而 supervisor 不再读取该流，存在漏检尾随的代码路径；这是 R06 已知缺口，B 正修，不宣称已实测复现。
- W2 先完整收流并确认进程/Job，再在 `:1308-1320` 调 W1 `read_response`。W1 `wasm_worker_protocol.rs:168-176,188-193,232-245` 限长、检查尾随、比较**完整 manifest**、校验阶段/索引/票数；这些错误在 W2 统一映射 `WorkerError::Protocol`。合法 `Outcome::Error` 映射 `WorkerRejected`；非零退出先映射 `IsolationUnavailable("worker_exit", Some(code))`。
- W2 累计 stdout 超 `MAX_RESPONSE + 4` 才即时 `Protocol`（`:1073-1078`），并在 EOF 后才解析前缀。因此声明超上限的 4 字节前缀后立刻关闭、正常退出，应返回 `Protocol`；只发该前缀并保持 stdout 打开，当前应走 deadline/回收，而不是假定已有前缀早拒绝。累计内容仍受上限与总期限约束；不把前缀早拒绝设为本轮新强制要求。
- `8937aed1` 新 `examples/wasm_worker_process_probe.rs` 已读完合法三帧+EOF，写合法 `Stop` 响应，直接 Win32 零字节 `WriteFile`，写 `x` 并退出；其新增测试只打印 `stdout_zero_read` 是否出现并断言 `Protocol`，尚未运行。若未观察到零读，只能证明尾随拒绝，不能称 R06 零读分支已实证。

## 建议的共用夹具和证据

只在测试构建/内部测试内构造 `TrustedWorker` 的固定 fault-probe 快照：扩展已提议的 `examples/wasm_worker_process_probe.rs`，并在 `src/wasm_worker_process_tests.rs` 加 `#[cfg(all(test,windows))]` 场景及必要 `tests/support/wasm_worker_process/`。这些属于 B 已签路径；不要新增生产 API、可执行路径/命令或由插件选择探针。独立 runner 在全部可能改 target 的编译后再次构建真实 worker 与故障 probe，分别复制受控快照并记录源/副 hash、PID、路径身份与提交树；不递归 cargo，不把 fault probe 冒充 W1。

probe 使用有界、预定义、仅测试内部的 mode，优先由其自身受控工件的 basename 从固定白名单映射（例如 `w2-no-read.exe`、`w2-partial-read.exe`）；同一 probe 二进制分别复制成独立快照并逐份核 SHA-256，不能用硬链接绕过现有单链接检查。此选择在读取 stdin 之前完成，因此不读/只读部分输入场景可达；未知 basename 固定失败。W02 需读取请求的模式再完整消费 3 帧及 EOF，回显 Host 实际 manifest 后仅修改目标字段。也可在测试内部已知合法 DTO 的字段上细分，但绝不能依靠先读取完整请求才选择不读 stdin 模式。模式只在测试例程/工件中使用，生产 `WorkerService` 不得新增由调用者指定任意路径、命令或环境的入口。合法控制同样经固定 fault 快照、合法 manifest、阶段和可用决策完成，先证明 fixture/Job/管道可达。为确定竞争点，可加 `cfg(test)` 的有界受控事件/屏障与记录回调；如果使用附加命名事件或控制数据，限定测试快照、唯一 nonce、最小 ACL/权限和等待截止，且不能改变生产句柄白名单或多出后台常驻进程。

每例必须能区分以下三层：

1. **到达故障点：**固定工件 hash/身份匹配；同一 PID 的 `created_suspended`、`resumed`；probe 实际消费帧数/字节、发出哪个片段或关闭哪个句柄的 test-only 阶段证据。无阶段/超前失败不算该反例通过。
2. **目标拒绝：**断言下面明确的稳定 `WorkerError` 类别/阶段（不必为每个 manifest 字段新增错误枚举）；禁止仅 `is_err`。5 秒总调用期限、2 秒回收窗口按实际固定参数计时并带可容忍调度上界，记录真实时间点；`CleanupUnknown` 是未证明回收，不等同正常 Deadline/Cancelled。
3. **真实收尾：**在 process 存活时由外部测试观察者仅取得 process 同步句柄，不额外持有 Job；通过安全屏障避免进程先退出导致未能取得句柄。错误路径额外 test-only 记录实际 process `WAIT_OBJECT_0`、同次 Job `ActiveProcesses==0`、各已发 OVERLAPPED 终态收割与句柄释放，之后证明 slot 可被后续合法调用复用；不能用调用返回或 `CancelIoEx` 返回代替证据。原有 `io_complete_reaped` 能证明正常 I/O 完成后 process/Job 回收，`failure_reaped` 仅覆盖 process/Job；都不能单独证明每种异常路径的 I/O 已收割。若同步系统调用/观察被故障注入卡住，先断言 `CleanupUnknown` 且同进程重建 service 仍 `Busy`，释放故障后真实收割并再测 slot 复用；禁止为了测试强行释放生产资源。

## W02 协议故障矩阵

| probe 做法（除注明外读完整请求和 EOF） | 必须目标结果与分支证据 |
| --- | --- |
| 合法身份、before 合法 `UseTools` 或 `Stop`，正常关三流并 exit 0 | 正控 `Ok`；同次 process signal/Job 0/I/O 完整及真实身份。之后每个故障都要证明到达其写入点，避免工件/启动失败掩盖 |
| 只写 1–3 字节长度前缀；或写完整前缀声明 N 再仅写 N-1 字节，关闭 stdout、exit 0 | `Protocol`；收齐/关闭阶段与 `io_complete_reaped` 后解析截断，分别覆盖短 header、短 payload |
| 只写 big-endian 0 或 `MAX_RESPONSE+1` 声明，关闭 stdout、exit 0 | `Protocol`；解析时长度无效；另测合法长度包裹非法 UTF-8、非法 JSON、未知字段/重复字段，均不得被接受 |
| 合法完整帧之后写 1 字节；或再写完整第二帧，然后关闭、exit 0 | `Protocol`；确认完整帧和尾随字节均发出并且 I/O 完成后解析，分别防忽略尾部与只处理第一帧 |
| 对返回 manifest 各自仅改 `version`、`request_id`、`nonce`、`binding_generation`、`plugin_id`、`plugin_version`、`digest`、`fuel`、`memory_pages`（均用格式合法的不同值） | 每字段独立 `Protocol`；输入由 Host 填入的真实 manifest 解析复制再单字段修改，避免一开始返回非法 JSON 而根本未触发身份比较 |
| before 返回 `Finish`/错索引/重复或乱序索引；after-no-tools 返回 votes；after-tools votes 数不等 | 每项 `Protocol`，须保持响应 manifest 完全正确；各场景输入合法，证明决策校验而非身份失败 |
| 正确 manifest 返回 `Outcome::Error { error: Module }` | `WorkerRejected`，且回收完成；不能记为合法 `Stop` 或成功决策 |
| 合法响应 + 退出码 42；再测无输出 exit 42 | 精确 `IsolationUnavailable("worker_exit", Some(42))`；当前检查顺序先退出码后解析，不能错误期待无输出 `Protocol` |
| 合法帧后**不关闭 stdout**并保持进程存活；或超限前缀后不关闭 | 在 5 秒 deadline 未采纳；回收已证实时 `Deadline`，若 2 秒内未证实则 `CleanupUnknown` 和 Busy。确认 probe 到达写帧/保持打开，不以启动失败充数 |
| 有效结果在截止后才尝试发送；以及进程已结束、在 supervisor 发送或接受之前由 test-only barrier 延迟跨过截止/取消 | 无成功决策；实际 worker 因截止被终止时记录 `Deadline`/回收或 `CleanupUnknown`；接收门测试需控制在 send/accept 边界并保留已完成进程证据，不能要求被杀 probe 一定成功写出迟到帧 |

## W03 管道故障矩阵

| probe 做法 | 必须目标结果与证据 |
| --- | --- |
| stdout 写完整合法帧 → 真实 `WriteFile(0)` → 写 `x` → 正常关闭退出 | `Protocol`，记录同 PID 创建、resume、`io_complete_reaped` 后解析；如果实际观测 `stdout_zero_read`，才称零读路径被覆盖；未观察时标记零读触发未证，只记尾随拒绝通过。不得用 Rust `write_all(&[])` 模拟系统调用（可能直接返回不写） |
| 对 stderr 真实零写后继续发少量字节，再关闭 | 验证 stderr 不因零读提前标 EOF；记录零读是否被观察及后续读取；若系统未呈现零读，记未触发。stdout 仍合法完整，不能以其他错误掩盖 |
| stdout 无穷输出，超过 `MAX_RESPONSE+4`；stderr 超过 64 KiB；各自分开 | stdout 精确 `Protocol`，stderr 精确 `IsolationUnavailable("stderr_limit", None)`；记录跨上限的字节量、各自管道已被读及真实进程/Job/I/O 收尾，不只依赖输出大小宣称通过 |
| 请求模块接近 8 MiB，probe 完全不读 stdin、保持进程及 pipe 打开 | 确认 Host 已到达 pending stdin write 和关闭前；deadline 后 `Deadline`（真实收尾）或 `CleanupUnknown`（未收尾仍 Busy），不得无界卡调用线程；记录已写/未完成操作及最终收割 |
| probe 只读可确认的一小段后关闭 stdin、但暂保活；请求大于 pipe 容量 | 确认实际读取量与关闭发生在 Host 尚未发送完之前，Host 的 write 提交或完成因 `ERROR_BROKEN_PIPE` 返回固定 `IsolationUnavailable("io_submit"/"io_completion", Some(ERROR_BROKEN_PIPE))`；若场景未能保证此顺序而先到 deadline，须标未触及而非把不同错误当通过 |
| probe 只写部分 stdout 后保持 stdout 打开；与前述合法帧不 EOF 分开 | deadline/unknown 及真实收尾；证明在等待 EOF 而非读取或创建阶段停住。只关闭 stdout 但还保持 stderr/进程活着也不能提前成功 |
| probe 在不读 stdin 时并行持续写 stdout/stderr，单项较小但合计压满各 pipe | 通过同步证据证明多方向均已在尝试且至少一个实际 pending；由于调度可能先命中 stdout/stderr 上限，预先为各子例选择唯一可判定的预期：受控超限例断言相应错误，受控不超限的阻塞例断言 deadline/unknown；不得仅按 `is_err` 或假设 pipe 容量 |
| 人为注入 GetOverlappedResult/事件观察失败、CancelIoEx 的 ERROR_NOT_FOUND 或迟到完成 | 必须显示已发生目标注入；不把取消成功/找不到请求当完成。若尚不能证明终态则 `CleanupUnknown`/Busy 并保留 OVERLAPPED、buffer、event、handle；解除注入后实际完成信号、process/Job 收割后才能复用。注入测试只是状态机证据，不称 OS 自然产生该错误 |

固定 W02/W03 的预期错误类型如实现后有明确合同调整，应由经理逐项裁决；测试要保留证据，不能为了绿色把超时、未知或启动失败合并为可接受的同一结果。`8937aed1` 的现有 W02 需要补同 PID 的 created/resumed/io-complete 及独立回收/slot 证据；现阶段尚未跑，不预判结果。

本轮未修改产品、暂存提交、构建或启动任何进程；未实测零读。W04–W06、真实 Host 强杀、生产绑定和恢复不在本矩阵通过范围。下一步等待经理转 B 明确实现和动态窗口，C 释放。
