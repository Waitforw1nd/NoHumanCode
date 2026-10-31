# NEXT-16 C 首次组件停止报告

2026-10-31，员工 C / 6.1 Terra，执行 ID C-R17-01-20261031-E01。依经理[首次组件放行](../../审查记录/第十七轮/2026-10-31首次组件放行.md)，本人实核并运行唯一指定组件一次。**编译成功，测试业务断言失败；未重跑。**

本人树 `../nhc-c-first-user-workflow`，branch `codex/c/first-user-workflow`；受测 HEAD `e810979b3eaffe701542e778ae7eddb3dc85b962`、NoManCode tree `7084f92095db7c1cb86fd799751bd7351aa6221f`、Rust tree `c636c0a81e9512a9700965abc4d472d9fcc43573`。前后 155 跟踪源码清单相同，SHA256 `585B97100AA7AECAD4922CA3E847ACCC9DF1D10DF49A3D9D949A6E4B74531737`；status/index/unmerged 空，源码未改。

Rust 工作目录 `NoManCode/rust-app`，实际原命令 `cargo test --locked --offline --test first_user_workflow -- --nocapture`；2026-10-31T02:15:44.7986002Z 开始、02:16:50.6612965Z 收口，cargo exit 101、外层会话 40584 exit 1 且已结束。离线、锁依赖，debug0/incremental0/jobs2，默认测试并发；独立树内 target/TMP/TEMP/log，既有 MSVC/SDK；无安装、付费 live、release 或 push。

原日志：编译完成，运行 1 测试，**0 passed / 1 failed / 0 ignored**。失败位置 `tests/first_user_workflow.rs:650`：checkpoint 恢复后 CLI `task resume` 预期错误 code `conflict`，实际 `request_failed`。日志仅输出断言值，未记录完整响应/HTTP status；不能因此断言具体错误分支已被现场证明。前面的文件读取、列举、搜索、写审批、Shell、预算、真实 diff、checkpoint 外部冲突、恢复与恢复回放断言已执行到位；失败后新 Turn、事件续接和重启部分未执行。失败过程中测试 `Host::drop` 仅对持有的子进程调用 start_kill，未输出 kill+wait 生命周期，因此不能宣称失败阶段子进程已 wait；临时测试目录由夹具析构管理，未提供可重现的持久 fixture 快照。

只读源码线索供经理/原 owner 判定：`store.rs:30` checkpoint 存在时 `ensure_resume_unsealed` 返回 `CheckpointError::Conflict`；`engine.rs:2092` 调用该检查；`server.rs:1743` 的 resume 只特判 `ResumeError::Sealed`，其它错误经 `classify_anyhow` 默认映射 `request_failed`。这可解释代码差异，但因为真实响应全文及前置分支未记录，属于待验证推断，不将其写成已确认根因。测试仍拒绝旧 Task 继续；错误分类与既有冲突语义是否须修由经理决定，不能自行放宽断言。

原始日志根为本人树 `.local/first-user-component-attempt1/`，驱动 `.local/first-user-component-attempt1.ps1`；原 test.log SHA256 `0F2FF4C12F3E3946E8CC3639BC21D096CC99DA035C03BE07815A001B8C9A3284`。5 对原件与发布副本逐对核验，2 份仅路径相对化，[清单](NEXT-16证据/组件attempt1/manifest.json) SHA256 `198F8E90FE6D110BF29A0598F5EDAFB82C205CFEB99DDA0026263BAD0E8327C6`。2026-10-31T02:19:33.0243455Z 只读扫描命令行关联本人树进程无匹配；这是事后观察，不证明失败时原子进程身份或 wait。

本次未改产品、公共契约、schema、权限或凭据；未重跑、不降低断言、不自行 APPROVED。持续 source/index clean/no-build 冻结，首个组件窗口结束，经理可安排 B。下一第一步等待经理裁决错误分类及精确后续任务；P07、新内核组合与五门禁均未通过。
