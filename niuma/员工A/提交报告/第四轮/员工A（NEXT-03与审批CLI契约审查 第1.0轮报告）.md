# 员工A（NEXT-03 与审批 CLI 契约审查 第1.0轮报告）

## 身份、基线与边界

- 任务：`A-NEXT03-04-AUDIT`；接收及审查时间：2026-09-26 02:49（Asia/Shanghai）。
- 执行者：员工 A；实际模型：`gpt-5.6-sol`。
- 固定基线：`e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`。
- 审查对象：员工 B《文件变更与安全恢复契约》、员工 C《审批CLI契约》，以及基线相关调用链。
- 共享工作树只写本报告和 A 身份；未操作 index，未修改产品源码、B/C 档案或经理文档。审查时 B/C 工作树仍在固定基线且没有可作为候选的源码差异。本轮未运行产品测试，不是实现验收。

## 总结

两份契约方向可实现，正文泄露、重复写恢复、后续编辑冲突、partial/unknown 及 CLI POST 禁止重试等核心要求合理。候选冻结前仍需修订下列协议缺口。否则实现可能通过局部测试，却无法稳定查询崩溃后的恢复事实，或对凭据安全作出超过现有 Host 的承诺。

## B Workspace 契约

### B0（上线阻断）：现有 Web 消费旧 DTO，契约却移除了对应字段

基线 `web/app.js:30` 用 `c.exists`、`c.previous` 展示“已存在/新建”和原内容字节数，并假定 restore 返回 `r.restored`。新契约有意移除正文并改 typed receipt；若只改后端，现有页面会显示错误分类和 `undefined` 字样，也无法正确展示 conflict/partial/unknown。

可操作修订：按经理有限授权，将 `web/app.js` 加入 B 唯一范围，仅适配新 DTO 的安全摘要、current/restorable 状态和 restore receipt；不扩 UI 设计。测试至少用真实新 DTO 验证 created/modified、conflict/unrestorable、complete/partial/unknown，页面不得引用 `previous/current` 正文或显示 `undefined`，partial/unknown 不显示“已恢复 N 个文件”的成功文案。

### B1（协议阻断）：unknown 恢复没有可寻址操作

契约要求恢复先持久 claim、逐文件 outcome、重启后 claimed→unknown，并让 partial/unknown 返回 receipt；但入口只有 `POST /api/tasks/{id}/restore`，请求没有 restore id/幂等键，也没有 receipt 查询接口。重启后调用方无法定位上一操作，重复 POST 也无法区分查询旧 unknown、创建新操作或幂等返回完成结果。

可操作修订：持久化稳定 `restore_id`，所有 `RestoreReceipt` 均返回它；由 `changes` 返回任务最新 receipt，或增加只读 GET 查询。明确同一任务存在 claimed/unknown 时，新 POST 返回现有 409 receipt，不创建第二次操作；complete 后重复 POST 返回同一 receipt；unknown 只能由未来显式 reconcile 处理，本切片不自动重做。

### B2（状态机阻断）：文件系统和 SQLite 不可能共同原子提交

SQLite 事务不能包住 NTFS 副作用形成一个原子提交。实际只能是 durable prepare/claim 提交，执行文件系统调用，再由数据库事务写成功事实和 approval finish。进程可在文件已变而 finish 未提交时退出，此时只能 unknown。契约中的“写后事实与审批 finish、结果、消息、投影在同一事务”还未定义“消息”，容易被误读为 provider 后续 assistant message 也在同一事务，基线 Engine 顺序无法满足。

可操作修订：冻结 `prepared -> fs_attempted -> finished|unknown` 状态机和每个 durable commit 边界。prepare 中的预期 after 摘要不是成功事实；只有 finish 事务成功才聚合为成功 change。列明 finish 事务包含本次 tool result、approval state、change outcome 及安全事件，排除 provider 后续消息。W07/W08 分别覆盖副作用前、系统调用返回后且 finish 前、finish 提交后等真实停止窗口。

### B3：unknown write 的展示和恢复规则不完整

准备记录不是成功变更，但 crash 后磁盘可能已等于预期 after。契约未冻结其是否进入 changes、`restorable` 值及可采取动作。

修订建议：未 finish 的 prepared 记录单列 `execution_state=unknown`，不聚合进成功 change；`restorable=false` 且返回安全原因码，不根据当前摘要猜测成功。保留受保护 before 供未来显式 reconcile，但本切片 restore 不消费 unknown write。

### B4：操作历史与当前文件状态需要拆开

“重复完成恢复幂等，文件后来编辑也不得覆写”意味着 completed receipt 是历史，不代表文件目前仍等于 before。

修订建议：分别定义 `restore_state`（never/claimed/complete/partial/unknown）和 `current_state`（matches_after/matches_before/missing/conflict/unreadable/unsafe）。完成后再编辑应返回原 complete receipt，同时 current_state=conflict，零写入。

### B5：hardlink 与安全替换语义需收窄

同目录 replace 能避免原地截断硬链接，但会改变文件 identity，且不保证 ACL、ADS、时间戳等元数据保留。Windows 对新文件和已有文件的替换调用也不同。

修订建议：本切片只承诺普通文件内容和存在性。检测 link count > 1 时固定为“replace 当前目录项并证明其他链接内容不变”或 typed conflict。ACL、ADS、时间戳和 identity 不在承诺内。临时文件应使用不可预测名称、`create_new`、同目录、flush/sync 后 replace；失败清理临时文件，清理错误不得把正文带入 API。

### B6：Host 内协调的边界需要与测试一致

单个 Engine 的 mutex 不覆盖第二个 Engine，也不覆盖使用另一 data-dir 指向同一 workspace 的 Host；基线 instance lock 只锁 data-dir。

修订建议：保证范围写成同一 Host 进程共享协调器；所有本 Host workspace 写入口都经过它。另一 Host/不同 data-dir 属于外部并发，只由 after fingerprint 检测 conflict，不宣称互斥。W09 至少使用同一 Host 的多 Store handle/并发入口，不只测一个 mutex 的两个 future。

### B7：恢复时的权限来源需冻结

恢复应同时满足任务持久 scope、成功 change 捕获时的规范化 workspace/scope 快照和当前 Host workspace 根；任一不一致即 conflict/unrestorable。请求体或可变 settings 不能扩大原审批权限。

### B8：补充1方向正确，经理已接受完整写封存规则

补充1要求同 task/path 再写前 current 等于最近 finished after，能阻止把用户或其他任务的中间编辑吞并为本 task 连续写。审查指出“完整恢复后禁写”还必须覆盖 claimed/partial/unknown，并需处理 complete/0 空恢复及两次校验。经理已接受以下冻结结论，契约落盘时应逐项可见：一旦创建 restore operation，其任意状态都封存该 task 的 write_file；无 changes 的 complete/0 是不创建 operation 的 no-op，不封存；连续写在 durable prepare 前和实际副作用前各校验一次，并置于同 Host 协调器内；错误固定为 typed `Unrestorable`，不保留 `Conflict/Unrestorable` 二选一。外部 TOCTOU 仍按原边界，不引入隐式新代次。

## C 审批 CLI 契约

### C1（安全边界阻断）：bootstrap token 不防同机进程

基线 `GET /api/bootstrap` 无 token；guard 只校验 Host、可选 Origin 和跨站头。任何能连接 127.0.0.1 并发送正确 Host 的本机进程都能取得 token。CLI 的 GET→POST 能复用现有浏览器跨站防护，但没有建立“只有 CLI 用户可批准”的认证边界。

可操作修订：明确 token 防浏览器跨站写和误请求，不防本机进程；本轮不新增用户认证。CL07 证明 no-proxy、no-redirect、固定 127.0.0.1、Host 对应端口及 token 不进入输出/错误即可，不宣称抵御本机恶意客户端。若需本机进程隔离，应另立 token 文件 ACL、命名管道或 OS 身份认证任务。

### C2：服务参数与客户端子命令的冲突未冻结

契约只规定 `--check` 加子命令、`--json` 无子命令拒绝；没有规定子命令与 `--data-dir/--legacy-data/--workspace` 的组合。接受并忽略会误导用户，初始化则违反零文件副作用。

修订建议：客户端子命令显式拒绝三个服务路径参数，exit 2 且零文件副作用；客户端 `--port 0` 也拒绝。冻结全局参数位置并使 help 示例与 clap 行为一致。

### C3：POST 超时必须表达“决定结果未知”

“有限超时”不足以稳定验收。approve/deny 在请求送出后超时，决定可能已经持久化；禁止重试是正确的，但不能提示“未生效”。

修订建议：固定 connect timeout 和总 timeout。POST 发送后断开/超时统一输出本地 `decision_result_unknown`、exit 1，并提示用 get 查询；不得自动 POST。连接建立前失败可用 `host_unavailable`，但不能回显 reqwest URL/error chain。

### C4：JSON 本地错误 envelope 尚未冻结

成功结构已大致明确，失败却同时提到 ErrorBody 和 `error=message`。传输、超时、非 JSON 等本地错误也需同一机器可读形状。

修订建议：冻结 JSON 失败的确切字段集，例如 `error/code/message/retryable`；HTTP body 仅白名单反序列化，非 2xx 缺字段或任意 HTML 不回显原 body。明确成功 DTO 对未知字段是忽略还是拒绝，避免服务端前向扩展使旧 CLI 意外失败。

### C5：ID 校验应直接复用共享函数

基线 `peachsh::secrets::validate_persisted_id` 已公开，approval HTTP 正在使用。契约的“或等价”容易产生长度、控制字符和秘密样式漂移。

修订建议：task/approval ID 均先调用该函数，再构造固定 path。覆盖 `/ ? # %2f \\`、控制字符、128/129 长度和秘密样式；非法值在任何网络请求前 exit 2。服务端 task path 当前未做同等校验，应记录为既有边界，不能成为 CLI 放宽理由。

### C6：无等待者批准是测试事实，不是 CLI 可判断状态

CLI 无法可靠判断是否有 runtime 等待者。它应始终忠实打印 ApprovalDto 的 status/execution_state，不输出“已执行”。CL03 的“只决定不执行”应仅作为测试安排和 Host 持久事实断言，不成为 CLI 分支或文案。

### C7（输出安全阻断）：不能转发任意服务端 message 或成功响应额外字段

CLI 即使不回显原 body，若把 mock/异常 Host 的 `ErrorBody.message` 原样打印，或只检查成功响应三个字段后把原 JSON 重序列化，仍可把 bootstrap token、敏感 echo 或任意额外字段送到 stdout/stderr。固定 loopback 并不使响应成为可信展示文本。

可操作修订：bootstrap token 必须传入所有错误清洗路径；HTTP 错误只接受稳定 code/retryable，并按批准 HTTP 契约映射固定本地文案，至少对 token 和敏感格式做 scrub，绝不转发未知字段或原始 message/body。成功列表和单卡必须完整反序列化到字段白名单 DTO，再从 typed DTO 序列化；不能在原 `serde_json::Value` 上删少数字段后输出。CL09 加恶意错误 message、顶层/嵌套 token echo、成功 DTO 额外秘密字段以及人读/JSON 双模式断言。经理已向 C 明确提出该修订，候选审查以实际落盘契约和固定 SHA 为准。

## 第二阶段入口

经理固定候选 SHA 后再审，不轮询变动工作树：

1. B：schema 7→8、prepare/finish/recover、restore_id/receipt、首次 before、after conflict、事件/HTTP/UI 正文泄露与真实 kill-window。
2. B：reparse/hardlink/replace、并发屏障、多文件 partial/unknown、旧 backup 不可恢复。
3. C：main 分派先于所有本地副作用、no_proxy/no_redirect/timeout/loopback、POST 不重试及 token/body 清洗。
4. C：真实二进制 stdout/stderr/exit code、参数冲突、ID 注入、POST unknown 文案及默认服务回归。

本轮仅执行只读源码/契约核对、`git status`、`git rev-parse`、`git worktree list` 和 `git diff --check`；未运行 check/test/fmt/clippy/wasm-check。

## 作者更正（2026-09-26 02:58，补充契约限定确认）

原“审查时 B/C 工作树仍在固定基线且没有可作为候选的源码差异”表述不准确。正确事实是：B/C 均已在各自工作树实施并存在未提交源码差异；两棵工作树的 `HEAD` 当时仍为固定基线，经理尚未提供固定候选提交 SHA，因此 A 没有把这些在途差异作为第二阶段候选证据。该更正不改变“不审变动工作树作最终结论”的边界。

经理随后落盘 B 补充2与 C 补充1，限定差异核对结果如下：

- B 补充2完整承接 B1～B7；补充1已承接 B0/B8。生产协调保证仅限生产 Host 的单个共享 Engine 协调器，不要求或宣称同一 DB 上多个活跃 Engine/Host 并发支持；原报告 B6 中“同一 Host 多 Store handle”不能扩张为多 Engine 验收要求。跨 data-dir/Engine 只依靠指纹冲突检测，并保留 TOCTOU 边界。
- B 已冻结稳定 restore_id/latest receipt、空恢复 no-op、prepared/finished/unknown、恢复历史与 current 分离、hardlink replace、权限快照以及 restore operation 任意状态后的写封存。未发现补充2内部新矛盾。
- C5 更正为：直接复用 `validate_persisted_id` 后进行 URL path segment 编码；`/ ? # %2f` 等若共享校验允许，应安全编码并得到对应 404，不强制本地拒绝。dot/dotdot 必须拒绝或证明不会被 URL 规范化到其他端点。长度、控制字符和敏感样式仍在联网前拒绝。
- C7 更正为：正常 Host 的结构化 ErrorBody 忠实保留 code/message/retryable/error，不强制重映射固定文案。安全要求是已知 bootstrap token 或可识别秘密一旦在任意响应字段回显即拒绝为固定 invalid response；成功响应必须完整 Approval DTO 类型/枚举校验并仅输出白名单字段，未知额外字段丢弃。该规则不承诺识别任意未知秘密。
- C 已冻结显式服务路径参数冲突、port 0、参数位置、2 秒连接/4 秒总超时、`decision_result_unknown`、本地 JSON error schema 和无等待者测试边界。未发现补充1内部新阻断。

本次只读查看两份补充契约和两工作树 `git status`/`git diff --name-only`，未阅读在途实现内容、未运行产品测试。后续仍等待经理固定候选 SHA。
