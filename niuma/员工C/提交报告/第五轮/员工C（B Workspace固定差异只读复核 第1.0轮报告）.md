# 员工C（B Workspace固定差异只读复核 第1.0轮报告）

2026-09-26；实际模型 `gpt-6-sol`。经理在 C 崩溃测试固定后追加本只读任务。固定范围是 B `083eae51820a008d02197c58b0dcfdf2ff7ba775..948e0cccf4f852b265c38a08c6b516470a0476e5` 的**实现差异**，重点 F6 recover、F7 原调用预检、F8 SQL 字面量、F9 损坏数据、AP10 有限适配；不自审 C 的 `workspace_crash.rs`。只读 `git diff`/源码与已完成的组合定向结果，不修改 B 源码、B 测试、共享暂存区，也不以本报告代替经理最终 review。

## 阻断发现

**B-F10 [P1] preflight 已持久 prepare 后的非 SQL 失败可能被记作已完成拒绝。** `src/engine.rs:842-884` 的新闭包先 `prepare_workspace_change` 提交 prepared，再调用第二次 `workspace::read_safe_file(&target)?`。若这次读取因权限/IO/路径变化失败，闭包直接返回非 `rusqlite::Error`；外层分支调用 `finish_approval_with_change(..., None)`，持久审批 finished、tool 结果“文件未修改”，却未将刚提交的 change 标为 failed 或 unknown。此时 `changes` 可见 prepared，重启又变 unknown，审批/工具结果与变更事实矛盾；任务后续恢复和同任务写入会被阻断。旧候选在此处直接向上传错，不会新增“审批已 finished 且 prepared 留存”的组合。建议显式跟踪已经 durable prepare 的 change，在第二次读取失败时按已知未执行文件副作用终结为 failed；如果无法确认，则按 unknown 统一收口，不能返回普通已完成拒绝。注入该窗口核对 approval/tool/change/重开事实。经理已将此项派 B 修复，编号 `B-F10`；本报告不声称有运行反例。

## 固定差异复核

- **F6 recover**：`src/store.rs:1350` 附近在同一 recover 事务中先把 claimed restore 的 change.restore_state 改 unknown，再把 claimed outcome 改 unknown；已完成 outcome 保持 complete、change 保持 restored。C 的真实 HTTP 子进程后副作用/outcome 前终止测试在 B `083eae5` 上 `pending` 失败、B `948e0cc` 上通过，并验证稳定 restore_id、逐路径 unknown、restorable=false 与重复 POST 409。此固定项闭合，前 FS 窗口仍归 B 单独补证。
- **F7 原调用绑定**：`src/engine.rs:1310-1364` 从持久 task 的 assistant tool_call 取 name/args，规范 path/path_key，重算审批 binding 与 args digest，校验 record 的 path、key、after digest；与原 task scopes、workspace、approval 也比较，差异在 claim 前 fail closed。`approval::validate_call_history` 已限制 tool_calls 只能由 assistant 给出且 ID 唯一。实现方向覆盖 A 原先指出的复制 digest 信任缺口；本轮没有独立运行 path/content/binding/call 各伪造夹具，不把代码审阅写成全项实证通过。
- **F8 SQL 字面量**：`src/repository.rs:133-155` 的 normalize 只在非引号区去空白/折叠 ASCII 大小写，单/双/反引号内容原样保留并处理成对引号；`'created'` 等 CHECK 值不会因规范化被折叠。新增伪造测试意图正确，但经理已发现该测试的缺表失败可在未证明目标 CHECK 变化时提前拒绝；这是已交 B 的证据缺口，不重复列为本轮新发现。需待 B 修订夹具证明唯一拒绝原因是字面量不同。
- **F9 损坏数据**：`src/store.rs:1380` 附近对 change kind/state/restore_state 使用明确枚举拒绝，`src/workspace_changes.rs` 增加 64 位小写十六进制摘要验证；`src/engine.rs` 在 changes/restore 入口把无效 finished after_digest 或解码错误映为 Corrupt，发生在恢复 claim/文件副作用前。现有测试对 NULL after_digest 有反例。此结论仅覆盖这些新增变更，未全面审计所有历史 receipt 字段。
- **AP10 有限适配**：`src/engine.rs:824` 的新写入 preflight 在文件副作用前拒绝会被持久脱敏改变的 args；`tests/approval_gate.rs` 改为断言敏感参数不写文件、无新 change、安全工具错误与事件不泄露。普通参数仍走已批准 write_file。F10 是该新错误分支在 prepare 之后的异常收口缺陷，故 AP10 适配不能在本 SHA 整体标成完成。

差异范围为 B 既有授权源码/测试六文件，`git diff --check` 与 C 组合定向结果未见范围外源码修改；后者只证明 W07/W08 的四个实际崩溃场景，不代替 B 全量 W01-W14 或五门禁。固定 `948e0cc` 结论：**CHANGES_REQUIRED（B-F10 + F8 夹具证明待补）**。B 后续提交须针对这两处差异再复核；本报告不预判后续 SHA。
