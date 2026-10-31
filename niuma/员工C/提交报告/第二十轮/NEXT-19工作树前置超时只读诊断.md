# NEXT-19 工作树前置超时只读诊断

2026-10-31，员工 C / 6.1 Terra。任务：[经理派发](../../../项目经理/任务/2026-10-31NEXT-19工作树前置超时只读诊断.md)。结论：N19-F01 与 N19-F02 的共同根因尚不可证实，必须保留两个独立问题；现有材料不足以指定生产修复点。两次均未观察到 `ClaimCommitted` 探针，但不能据此确定同一阻塞位置或原因。

## 基线和范围

B attempt4 受测候选 `a7fe2626b36c130937820685f4f2d38612094f5d`，NoManCode tree `8daca32509d9d4ece839ec15ef6d3efb22427932`，Rust tree `e686054a56772c4bea611efb3e37754a0b0c94ca`；C 预备树 `codex/c/tool-call-workflow` 为 `73a03b6b646e0c1cace4d0091807d03d30af3830`、`8c189cc7aeabb92af95e340c6449394058048b5b`、`535566761693515f6087a1a306385487c580518d`。本人核得两树 status、index 和未合并项为空。B 树只读取证，没有修改其文件或 Git。B 原件根相对仓库根为 `../nhc-b-tool-call-application/NoManCode/rust-app/.local/next19-b-a7fe2626-attempt4/`；全文读取 `03-lib.stdout.log`、`03-lib.stderr.log`，并读 `03-lib.json`、runner log 和 W08 的 `tmp/.tmpyhZH0h/lifecycle.jsonl`、`child-claim.log`。完整 lib 353 passed / 2 failed / 0 ignored、exit 101；check/build 0，后续 approval_gate、Clippy、fmt 未执行，均为既有运行结果。

## 共同路径及静态边界

B 固定代码 `src/engine.rs:974` 起：规范化、Store replay、inventory/roots 与项目配置、GitContext 和 parent identity，依次 await `git::prepare` 和每个项目 `git::probe_project_git`，随后 `worktree_aux`、`claim_worktree`；`src/worktree_management.rs:637` 起的 SQLite IMMEDIATE 事务提交 receipt/reservation 后，Engine 才 spawn worker，worker 调用 `ClaimCommitted` 探针。提交完成与 worker 调度、探针执行之间仍有窗口。

`src/git_worktree.rs:586` 的 `inspect_with` 顺序执行多项 Git 检查、文件系统与身份检查；`prepare`（795）还执行目标检查及 `branch_available` 的 `show-ref`（846），项目 probe（762）可再次调用 `inspect_with`。每条 Git 调用各有 10 秒 deadline（437），其运行还等待输出管道及子进程，失败清理最多另等 5 秒（278）；测试从等待 ready 到超时整体也是 10 秒。多项顺序前置的时间可能累积超出观察窗口，但这只是静态可行路径，不是本次任何命令耗时、超时或负载的证据。Store、同步文件系统、Git 子进程、异步调度及其他等待均缺阶段观测，无法排定或排除。

从主仓只读比较 `73a03b6b646e0c1cace4d0091807d03d30af3830..a7fe2626b36c130937820685f4f2d38612094f5d`：Rust 改动包括 `engine.rs`、`store.rs` 等；`git_worktree.rs`、`worktree_management.rs`、`server.rs` 未出现在该限定 diff 中，Engine diff 中未检出上述 worktree 路径的直接变更。此比较不证明共享状态、运行资源或生产行为没有回归，也不能据此归因 NEXT-19 补充03。

## N19-F01：W08

原生命周期显示 listener 通知已观察，父进程等待 ready 10 秒超时，`original_http_finished=false`；之后析构 kill child 并 reap exit 1，随后才记录 HTTP transport error。kill 后错误不能反推出超时前连接或 handler 的状态。child 日志仅显示运行一个测试；未见 ready 文件。

`src/server.rs:3207` 起的 child 在写 listen 通知前打开 Store、恢复、建立 Engine 并注册探针，之后进入 `axum::serve`。listen 通知不能证明 POST 已被服务端接收。父端 `server.rs:3410` 起并发发真实 Router POST 与等待探针文件；等待只观察请求是否完成。handler（1250）严格解析后 await Engine。超时前 HTTP 未结束不能区分尚未派发、guard/handler、Git 前置、事务或 worker 调度；不能从它证明 handler 已进入。

经理[只读查询记录](../../../项目经理/审查记录/2026-10-31NEXT-19-attempt4-W08只读诊断.json)对独立 query 副本记录 WAL 0 字节、projects 1，其余 tasks、worktree_operations、reservations、origins 为 0；仅说明该保全快照未观察到持久 claim，不能定位请求或具体步骤。本人未打开或恢复原 DB/WAL/SHM，也不将该结果用于 N13。

## N19-F02：N13

`src/worktree_management.rs:1293` 起由独立 fixture 创建 Engine，`ClaimCommitted` 回调发送 ready；10 秒等待返回 `Err(Elapsed(()))`。测试持有的 Engine Arc 保存 Mutex 内的 probe 闭包，因而 sender 仍可存活：即使 caller 在 claim 前提前返回错误或 panic，接收端也可能继续等待并超时。原测试未在等待期间观察 caller JoinHandle；所以不能仅凭 Elapsed 判断 caller 仍在执行，也不能宣称已证实提前返回。`caller.abort()`、worker 保活、reservation 与最终注册等后续断言均未执行。

fixture 的 TempDir 未在原日志中输出可唯一关联的目录；不可将其他 `.tmp` 或子进程现场当作 N13。没有该次数据库、caller 返回/JoinError 或逐阶段时序，无法判断早返回、等待、提交后未调度等情况。

## 建议经理裁决的最小取证

建议先按现有文件职责和正式任务单明确唯一 owner：W08 HTTP/测试可由原接口测试 owner 负责，worktree/Git 与 N13 可由工作树 owner 负责；共享 `engine.rs` 的写入人由经理明确。上述仅建议，不在本轮派发或实施。

1. 在单独、明确授权的诊断候选中，使用仅测试构建、每 case/fixture 独立关联 ID 的有界追加阶段事件，记录单调时刻和安全的进程/任务标识；覆盖 HTTP 发起/服务端入口及固定分类出口、Engine 各前置、prepare、每项 Git 命令开始/结束及子 PID、退出/超时类别、项目 probe、claim 进入与 commit/rollback、spawn 和探针进入。Git 输出只记有界长度与分类，不记录原文、凭据、任意输入或路径；事件写入失败应可见。探针避免额外持有业务锁或引入业务 await。
2. 对 N13 同时观察 ready 与 caller JoinHandle，确保超时前完成的 caller 被记录为固定安全结果或 JoinError；不能让诊断观察提前取消 caller 或改变原 abort 时机。为该 case 唯一标识并保全 fixture，必要时先原样保留 DB/WAL/SHM，再对独立副本查询，分别区分提前结束、前置等待、已提交但 worker/探针未运行。要记录清理/丢弃边界，不能认领无标识目录。
3. W08 关联请求发送、服务端入口、固定状态应答、原 child 与 10 秒时刻的请求状态；保留真实进程 kill+wait 和既有断言。事件需要足以区分请求未到、进入 handler、前置/claim 和提交后 worker 四类窗口；诊断本身不能改变生命周期。

若经理批准一次限定诊断，应事先固定候选、owner、日志保全与运行窗口，保留原 10 秒、原测试并发和断言；事件无结果时继续报告未知。不得通过提高 timeout、全套串行、削弱断言或循环重跑补绿，也不自动合并旧 NEXT-15 W08 或 N18-F01。资源调整须先有具体证据与验收设计。

本轮未修改产品、未编译、未运行测试、未重跑、未执行 Git 写入或打开恢复原 DB；仅写本报告及本人身份。只读诊断结束，等待经理审阅和后续明确派发；不自行实施修正或取证。
