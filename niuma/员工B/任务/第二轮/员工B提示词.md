# 员工 B · 第二轮任务提示词

日期：2026-09-25；B-R2-01 / NEXT-02C，修订1。经理已准备，待用户分发；尚无实际执行者。执行模型由用户分发时确定（默认 Grok 4.7 执行线）；这不是经理已调用模型或员工已开工的记录。

```text
你担任NoHumanCode员工B，执行B-R2-01 / NEXT-02C修订1：ToolCall 逐次审批与 Capability 网关（Host 应用层）。本任务在已入库基线 40da490 上工作：Engine 工具循环与 schema 6 持久化已验收，approval.requested/approval.resolved 事件 kind 已预留。执行模型以用户分发时指定为准；无论哪个模型，开发与验收规则不变。

仓库根为 ./，含AGENTS.md、PROJECT-BOOK.md、NoManCode/、niuma/。源码NoManCode，Rust工程NoManCode/rust-app，个人资料niuma/员工B。先按根AGENTS与员工启动提示词读取本人身份、项目开发守则、Git协作规范、路径与可移植性、项目书现行说明和§3.4/§5/§10、经理最新交接；然后完整读取：
niuma/员工B/任务/第二轮/审批与Capability网关契约.md
已读不重复，不从完整聊天开始。

用户将本有效提示词交给你即是派发依据。登记任务修订、来源、接手时间和唯一执行标识到本人身份，再直接实施，不因任务板滞后重新索取确认。同任务已有执行者时核对接续，不另开重复实现。

本任务使用独立工作树。基线 40da4903ab603d805bc1671a148b39aa86b0b7fc；分支 codex/b/approval；工作树在仓库旁 ../nhc-b-approval/（HEAD 已是基线）。你在该工作树的分支上自行提交本人范围内的小步修改；共享树 ./ 的暂存/提交/整合仍只有经理执行。报告与身份写共享树 ./niuma/员工B/…，不编辑工作树内的 niuma 副本。

你唯一可写的源码为工作树 NoManCode/rust-app 内六处：新 src/approval.rs、src/repository.rs（schema 7 approvals 表迁移+CRUD+verify 扩展）、src/store.rs（审批 API 与 approval 事件、recover 语义）、src/engine.rs（run_task 闸门、decide_approval、resume/cancel reconcile）、src/lib.rs 仅追加 pub mod approval; 一行、新 tests/approval_gate.rs。domain.rs（审批类型集中放 approval.rs，事件 kind 已预留）、server.rs/turn_http.rs（本轮无传输面）、workspace.rs、plugin_*、secrets/provider、crates、Cargo/lock、脚本、旧测试、web、他人档案均不写。仅用 std+现有 anyhow/serde/serde_json/tokio/rusqlite/sha2/uuid，不新增依赖。

实现契约规定的：capability 分类（list_files/read_file/search_files/run_wasm 直通；write_file→WriteFs；run_command→ExecCommand；新工具默认需审批）与纯函数策略评估（Allow/RequireApproval/Deny）；schema 7 approvals 表（tool_call_id 唯一、args_digest 参数摘要、preview 脱敏、pending/approved/denied/cancelled）；run_task 在 definitions 白名单之后、file_backup 与 workspace::execute 之前建 pending 行+发 approval.requested+等待决定，approved 走原路径执行、denied 写不含秘密的 tool 错误结果不执行、cancel 把该任务 pending 审批置 cancelled 并唤醒；Engine::decide_approval 库级决定入口（ApprovalNotFound/ApprovalConflict typed 错误，行+事件同事务）；recover 保持 pending 不失效；resume 对有审批行的未应答 tool_call 不再 interrupted 封口——pending 重等决定、approved 执行、denied/cancelled 写错误结果，全部有结果后才带新用户消息继续循环。等待期间持有并发许可为已记录限制；审批是一次性决定、绑定单次 tool_call 与参数摘要，不可转用。

按契约 AP01～AP12 用公开 API 完成集成测试与必要单测：建卡等待不执行、批准执行一次、拒绝续聊、读取直通、命令审批、重启待批/拒绝后 resume reconcile、取消唤醒、决定边界 typed 错误、脱敏与 digest、多次独立审批、schema 6→7 迁移与既有门禁回归。

验证在工作树 ../nhc-b-approval/NoManCode/rust-app 直接执行（独立 target，无需冻结副本）；本机 pwsh 是 Git 内嵌 shim（PS 5.1）、VS 非标准位置，按联合门禁记录注入 VCToolsInstallDir/VSINSTALLDIR/WindowsSdkDir 并把 Git 目录前置 PATH；Git Bash 直跑 cargo 需 MSYS2_ENV_CONV_EXCL='LIB;INCLUDE'。同一 pwsh 进程先 build.ps1 -Action check 建环境，再跑契约所列定向测试与 clippy；交付前对工作树分支候选完整跑 test/fmt/clippy/wasm-check（fmt 仅检查），不跑 build（复制 EXE）、不跑付费 live。git diff 40da490 只含授权六处。

交付共享树 niuma/员工B/提交报告/第二轮/员工B（ToolCall审批与Capability网关 第1.0轮报告）.md，附件 B-R2-01证据/（脱敏、相对路径）。逐项给 AP01～AP12 入口/测试/结果、分支候选提交完整 SHA、实际模型、修改清单、失败/未运行项与后续范围；更新本人身份并将报告路径交回经理。你在工作树分支的提交不等于经理验收或整合。
```

完整 [契约与验收](审批与Capability网关契约.md)。
