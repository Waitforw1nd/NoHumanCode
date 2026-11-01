## 经理后续取证裁决（2026-11-01）

经理核对官方 Job 完成端口语义：NEW_PROCESS 表示进程加入 Job，嵌套 Job 消息可向父链传播；延迟收割后旧 PID 可能复用。PID 12952、21912 的真实身份、所属具体 Job 和产生原因仍不明，不称其为纯祖先噪声或特定程序。W04 目标 PID 的限额、回收和占槽结果可按范围核验；“仅一进程/无后代”未确认，登记为 **W06-P01：W2 整合阻断**。后续在新现场实时采集消息，持查询/同步句柄，核创建时间、映像、实际 Job 成员和进程列表/计数；不重跑未改变观测的旧组，不改限额或关闭审计。
# NEXT-21W2 补充03 W04 attempt7 动态结果

2026-11-01，员工 B。依经理批准的 W04 唯一窗口执行；独立树 `../nhc-b-wasm-worker-process`、分支 `codex/b/wasm-worker-process`，受测 `dbba89e90736696f1a9bc0bf4520495f260b1519`，产品树 `4c9b80eff52d0f338297717b51d8d767d9cf81ad`、Rust 树 `bee7ae8505d9878106228d00edb2cf1bd6282090`。runner PID 7164，现场 `NoManCode/rust-app/.local/next21w2-b-dbba89e9-attempt7/`。B 执行产品树提交和动态，经理负责核验及整合，主仓本人档案未由 B 暂存或提交。

## 实际结果

19 步全部 exit 0：workspace all-targets check、lib no-run、worker/probe 同次构建、四项 gate、真实 W01、七项 W04 exact、W02 与 W03 exact、fmt、workspace all-targets Clippy `-D warnings`。各 exact 单线程、各自独立 stage 及模式目录；逐步命令、退出码与源码清单均保存在本 attempt。W01 before PID 62388、8,388,544 字节模块，after PID 45952，均 exit0/process signal/Job active0/I/O 完成。

- 资源组：真实 worker 的 start/run fuel、memory.grow 和 manifest 声明边界六子例符合断言。CPU probe PID 60976，8 个进程内线程均记录启动，墙钟 3,658ms，实际 Job 限额消息2、非零退出和真实回收；实际用户时间 **40,156,250 个 100ns 单位，约 4.016 秒**，超过配置的3秒，不能将该配置称为绝对无超额的硬实时上界。内存 probe PID 62072，实际消息9、exit43、峰值 131,018,752 字节、实际分配失败码1455；所查询进程内存限额为 134,217,728 字节。消息和记账属于该次观测，W1 Store limiter 未被独立触发证明。
- R06 确定性注入：stdout PID 41740 和 stderr PID 33372 在注入零读后仍返回合法 Stop，stderr 后续字节确实读取。W03 回归的真实 stdout/stderr 零写均运行，但 `actual_zero_read_observed=false`；不可把注入覆盖写成自然零读命中。
- 取消/接受组通过预取消、创建/resume/submit 票据前后、caller future 真实 Drop、接受前后取消及接受前跨截止断言。七种预期注入 unknown 均在放行前确认 Busy，放行后实际 process/Job/I/O 回收且正控复用；持续事件错误 PID 16788 和完成结果错误 PID 58028 走实际错误分支，均维持占槽至恢复真实观察后回收。注入不代表自然 OS 故障概率。
- 成员关系异常注入 PID 25008 的记录完整，实际 process signal/Job active0 后回收；真实不同 Job 的 R04 后备测试 PID 57824，观察句柄 signal、真实 Job query1/active0、watchdog 未介入。两者各自证明的范围不同，不以单纯终止 API 返回冒充回收。
- W02 33 个故障模式和 W03 10 模式回归通过。W03 双队列压力 PID 63968 两路报告配额各16,384、可读各131,072，放读后 stderr_limit 并回收；仍未直接证明两个同步写系统调用在同一时刻均阻塞。

资源测试 IOCP 原始队列除目标 PID 外，还包含 NEW/EXIT 消息中的 PID 12952、21912；本轮未取得其身份或关联原因，不能只凭这些消息认定其是目标 probe 的子进程，也不将它们静默忽略。目标 CPU 和内存限额消息分别与 PID 60976/62072 关联；请经理核验原始 Job/IOCP 证据并裁决额外消息的含义。

## 快照与限制

构建后立即固定 worker 快照 SHA-256 `996354a20003bd9ecfb6fe5b6e353499861c17c8918ddc744efe9a9614d2fcf6`，probe `b9c2bbb309c4dbf949dfe2dcd358369b40fd7af521d5c0a270f4d3ab386c3c74`。两工件和实际独立模式副本共110项，结束重新散列无不匹配；189 条源码清单前后一致，清单文件 SHA-256 均为 `54af9ef32cb35430e8d5c7d125a8f8269c7c87039820d1d13e250ef28e7c4299`。预检/结束树 clean，结束无 heavy；预期注入 unknown 只有放行后的真实清理和复用通过，未把 API 返回或 Cargo 退出单独当回收证明。旧 attempt2–6 原件保留。

本报告交经理核证，不等同完整 W2 或生产接入通过。自然零读、同步双写同时阻塞的直接观察、W05 真实 Host 强杀、W06 工件/继承/非 Windows、正式信任入口、持久绑定/W3 与 A/P08 仍开放。

