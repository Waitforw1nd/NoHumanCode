# 员工 A：审批 HTTP 候选安全复核 第1.0轮报告

日期：2026-09-26。任务：`A-R3-AUDIT`。执行标识：`A-R3-AUDIT-20260926-01`。实际模型：`gpt-5.6-sol`。

受审固定候选为 `2af377026f83c0b8b3c6f24cf5a268bac07090ee`，基线为 `379e2aa6378bbe1161c9f65f6655f93876834ac0`。本轮只通过 `git show` / `git diff` 审查提交对象，没有读取 C 的变动工作树作为最终事实，没有修改 C 源码、测试、档案、target 或共享 index。

结论：**生产代码未发现阻断缺陷，但候选尚需补齐 AH09/AH11 两项验收证据，当前不能按完整矩阵放行。** 候选只修改授权的 `src/server.rs` 并新增 `tests/approval_http.rs`；三条 HTTP 路由、DTO、输入约束、typed 错误映射、真实恢复窗口和 SSE 重连实现与正式契约一致。经理已确认并交 C 仅补测试，不要求修改生产代码。该结论是固定 SHA 的静态安全与证据审计，不代替经理五门禁，也不由员工 A 宣布验收或整合。

## 1. 产品代码

- 路由准确增加 GET task approvals、GET approval、POST decision，沿用全局 Host/Origin/sec-fetch-site guard、写 token、1 MiB body limit与三安全头，没有增加 CORS 或旁路执行入口。
- `ApprovalDto` 是显式字段白名单，没有序列化 `ApprovalRecord`；不包含两个 digest、workspace、write scopes、allow commands或原参数。status/execution state使用冻结枚举，nullable字段保留。
- 三条路径都经过 `PathRejection` 和 `validate_persisted_id`；失败返回固定400，不回显 id。POST body使用 `deny_unknown_fields` 与两值枚举，仍由 `ContractJson` 处理400/413/415。
- task列表先调用 `Store::task`。真实缺 task 的 `QueryReturnedNoRows` 才经 `read_error` 为404；JSON或身份损坏为安全500。审批查询/决定遍历 anyhow error chain识别 `ApprovalError`，NotFound/Conflict/BindingConflict/UnknownResult/CorruptState分别映射404/409/409/409/500。
- 非 typed 错误不落到通用400，而是安全500。保留的 SQLite busy/locked cause才标 `retryable=true`；Repository已擦除cause的损坏或读取失败保持500且不可重试。错误文案固定，不输出 SQL、参数或底层文本。
- POST只调用 `Engine::decide_approval`，固定 `decided_by=user`，直接返回决定事务快照；没有复制 binding、调用 ensure/claim/finish/resume/provider，也没有等待或启动 worker。符合“binding POST不可达”的正式修订。

## 2. AH01-AH14证据强度

| 项目 | 独立核查结论 |
| --- | --- |
| AH01 | 真实 pending 前置条件后经HTTP查列表/单卡；批准前无文件、backup/start/result。另有存在Task空列表和未知Task 404。顺序与 Store 实际顺序逐项比较。 |
| AH02 | HTTP approve真实写一次并finished；deny零写、产生安全tool结果并继续完成；检查resolved一次与200决定快照。 |
| AH03 | 真实读取工具完成，审批列表空且无requested。 |
| AH04 | 真实PowerShell追加计数；approve后1、deny为0，不以spawn失败代替拒绝。 |
| AH05 | 旧current-thread runtime先观察pending再 `shutdown_timeout`；新runtime重建Store/Engine并recover。HTTP批准后provider计数和文件均不变，再显式resume执行一次；第三次重开仍finished、一次副作用和一次tool_start。不是drop Arc夹具。 |
| AH06 | denied跨真实runtime保持零执行；finished由AH05真实执行产生。unknown场景为真实命令先写计数再无限等待，关闭前共同观察计数1、claimed、零tool_result；shutdown后新Store recover为unknown，GET 200、重复决定409、resume失败、计数/一次start不变且非completed。没有手造finished或unknown。 |
| AH07 | pending经公开HTTP取消后决定409、零执行；claim先发生用真实命令屏障，取消不改批准且真实结果finished、下一工具不执行。另以Barrier并发HTTP cancel/decision，resolved恰一次。 |
| AH08 | 8个Barrier并发approve/deny，断言恰一200、其余409、决定/decided_by正确且总resolved只增一次；重复与反向决定均409。 |
| AH09 | DTO字段集合精确比对，响应扫描mock秘密和禁止字段；command preview字符上限。但 requested/resolved 审批事件只检查approval_id与归属，没有直接扫描事件data中的mock凭据、正文、原参数或字段边界。**证据待补。** |
| AH10 | 两个真实worker复用同call id，生成不同审批id，分别approve/deny且只首个写入；列表按task隔离。 |
| AH11 | HTTP覆盖缺失/重复/额外字段、类型/null/枚举/语法、缺CT、超大body、guard/token、非法path、unknown与坏Task。独立SQLite连接 `BEGIN IMMEDIATE` 真实触发decision busy 500 retryable；关系损坏GET为500不可重试。但通用error helper未断言 `code` 与固定安全message；超大body和Host/Origin/sec-fetch拒绝发生在记录已approved后，不能直接证明fresh pending零变化。**证据待补。** |
| AH12 | seed的无worker审批经POST只变决定、文件不写；重复决定409。AH05进一步证明重启无worker的真实HTTP批准不启动provider/工具。 |
| AH13 | Run和Session两条真实SSE分别消费requested，逐帧检查SSE id等于JSON seq/cursor及task/session/turn/approval归属。关闭已消费流后HTTP批准，用实际消费seq分别以`after`和`Last-Event-ID`重连，所有帧必须大于游标且最终取得同一resolved；resolved持久事件一次、工具仍执行完成。 |
| AH14 | GET unknown 200、再决定409不可重试、resolved不增加；binding摘要篡改后POST允许200，worker claim前fail closed且零start/result。BindingConflict/UnknownResult mapper另以typed chain单测覆盖，没有伪称HTTP决定可达。 |

## 3. 必须补齐的证据

1. AH09：以含mock secret正文/命令参数的真实审批，取得 requested 与 resolved 事件，序列化断言不含mock凭据、正文、原command或 `arguments`，并核对审批事件data只含契约允许的安全摘要/决定元数据。扫描范围应限定审批事件，不能扩大为“整个SSE无正文”。
2. AH11：对400/403/404/409/413/415/500代表响应断言准确 `code`、固定安全message、`error == message`，并确认不含路径id、SQL或mock secret。用fresh pending验证超大body及Host/Origin/sec-fetch拒绝后status仍pending、resolved/副作用零新增。

以上是验收证据缺口。固定 SHA 的生产代码静态路径显示middleware/extractor在handler前拒绝，审批事件由B层安全摘要构造；因此本轮没有把缺测试夸大为已复现产品漏洞。补证如揭露行为不符，再按实际结果升级。

## 4. 其余限制与非阻断观察

- 本轮没有重复运行 C 已保存的14项定向测试，也没有使用 C 的 target；`git diff --check` 无输出。运行通过数及最终五门禁应以经理保存的固定 SHA 日志为准。
- AH13 在同一会话/任务上证明严格递增与不重放，没有专门插入另一会话制造全局seq间隙；基线 `turn_http` 已覆盖Session过滤与seq允许间隙，本候选没有修改SSE实现。两份证据合并足以支撑本次接入范围。
- AH06 的shutdown是Tokio runtime停止与 `kill_on_drop` 子进程终止，不是操作系统断电；这是既有审批安全模型接受的测试边界。夹具在停止前验证了真正的claim和真实外部副作用，并在重开后验证unknown，因此不存在用failed/手造状态替代目标窗口的假阳性。

## 5. 交接

本轮要求 C 仅补上述测试证据并形成新固定 SHA。新候选可对差异做定向复核，无需重读未变化的产品实现与其余矩阵。经理仍应以新固定 SHA 五门禁结果决定是否放行；本报告不能覆盖后续运行事实或新提交。
