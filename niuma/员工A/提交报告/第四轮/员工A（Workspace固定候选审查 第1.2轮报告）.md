# 员工A（Workspace 固定候选审查 第1.2轮报告）

## 身份与边界

- 任务：`A-NEXT03-04-AUDIT` 第二阶段 B 中间候选审查；实际模型 `gpt-5.6-sol`。
- 基线：`e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`；固定候选：`dd57adfc9321454651bdcdabb3a081bc95091600`。
- 候选恰好修改契约授权的 11 个文件；`git diff --check` 无输出。审查开始时工作树 HEAD 为该 SHA；其后 B 已继续未提交返工，A 未把在途内容作为本报告证据。
- 本轮只用 `git show`/`git diff` 审生产状态机、事务、schema、path/权限、unknown、HTTP/UI 和现有测试。未改 B 源码、target、Git index，未运行产品测试。
- B 已报告 check exit 0、`workspace_changes` 4/4、AP19 2/2、node exit 0；B 同时明确 W02/04/06～10/12～13 夹具与五门禁未完成。这些属于员工已承认的未完成证据，不冒充 A 新发现或正式失败日志。

## 结论

候选已建立 schema 8、受保护 before、prepare/finish/unknown、首次 before 聚合、after 指纹冲突、restore receipt、Host gate、安全事件 DTO 和有限 Web 适配的主体结构。经理早检 BP01/02/03/04/07 的代码主问题已关闭；BP05/BP06 只部分关闭。

该中间 SHA 仍不能进入验收。下列八项是固定候选中的实际实现缺陷，其中恢复持久化失败、临时明文残留、失败记录污染和 legacy/new 基线混合直接影响安全恢复保证。B 已知证据未完成项另列，不用测试缺失掩盖代码问题。

## 实现缺陷

### [P1] 文件副作用后数据库失败没有落 unknown receipt

位置：`src/engine.rs:1279`、`src/engine.rs:1291`、`src/engine.rs:1295`，`src/store.rs:883`、`src/store.rs:896`。

恢复动作成功后，`finish_restore_path` 或最终 `finish_restore` 的 SQLite 更新若失败，`?` 直接返回裸错误。当前请求得到普通 500，没有持久化/返回 unknown receipt；磁盘已经变化，但 operation/outcome 仍可能是 claimed。下一次请求才可能把已有 claimed 映射为 unknown，无法满足本次调用“副作用后故障立即明确事实”的要求。

修复要求：文件副作用开始后，任何 outcome/parent finish 失败都走 best-effort terminal unknown/partial 持久化并返回安全 receipt；若连 unknown 持久化也失败，响应仍应是固定 unknown 语义且不得泄内部错误。故障注入必须命中 action 成功后的 outcome commit 和最后 parent commit 两个窗口，并核磁盘、DB、HTTP、重启事实。

### [P1] partial/unknown 没有把剩余逐路径 outcome 置为明确终态

位置：`src/engine.rs:1274`～`1290`，`src/store.rs:896`。

第 N 个路径 action 失败时只更新 parent restore 为 partial/unknown；失败路径和未执行路径仍保持 claimed，对应 `workspace_changes.restore_state` 仍可能是 pending。`changes()` 不结合已有 terminal receipt 封存全部条目，因此 partial 后剩余路径可显示 `restorable=true`，但同 task 又因已有 restore operation 永久禁写/禁新恢复。API 同时表达“可恢复”和“任务已封存”，状态矛盾。

修复要求：terminal parent 事务同步把失败/未执行 claimed outcomes 转为明确 unknown/conflict（需要区分可增加安全状态），并使该 task 的全部 change 在任何 restore operation 后 `restorable=false`；历史 receipt 与 current state 继续分离。

### [P1] atomic_replace 在 replace 前失败会遗留明文临时文件

位置：`src/workspace.rs:32`～`51`。

临时文件创建后，`write_all?`、`sync_all?`、目标存在时 `read_safe_file(path)?` 均可提前返回；只有 `replace_path` 失败分支执行 remove。restore 会把 DPAPI 解密后的 before 明文写入 `.peachsh-*.tmp`，故障时可在 workspace 留下受保护快照正文；普通 write 也会留下用户内容。

修复要求：create 成功后立即建立 RAII cleanup guard，所有错误返回均删除 temp，replace 成功后才 disarm。注入 write/sync/pre-replace validation/replace 故障，证明无 temp 文件和正文残留；清理失败只记录安全内部错误，不回显路径或内容。

### [P1] failed prepare 会永久污染后续成功变更和恢复

位置：`src/engine.rs:1021`～`1034`、`src/engine.rs:1176`。

聚合时若同 path 的首条记录为 failed、后续为 finished，代码只更新 `after_digest`，不会把聚合 state 改为 finished；restore 更在聚合前拒绝任何非 finished 记录，把 failed 也映射为 Unknown。一次真实写入前/执行失败的准备记录会让同 task 后续成功 write 永久不可恢复，违反“失败准备不算成功”。

修复要求：failed 记录不参与成功聚合或恢复目标；unknown/prepared 仍使 task fail closed。测试覆盖 failed→finished 同 path、finished→failed 同 path，以及失败无文件副作用/无成功 change 计数。

### [P1] legacy backup 与新 change 混合时可恢复到任务中间态

位置：`src/engine.rs:1161`～`1173`。

代码只有在新 `workspace_changes` 为空时检查旧 `file_backup`。旧任务已有 legacy backup 后升级并产生新记录时，records 非空会完全忽略 legacy 事实；新记录的 before 是旧写之后的当前内容，restore 可把文件恢复到中间态，而不是首次 task 前镜像。契约要求无可信新记录的旧 backup 不可恢复，不能把混合历史悄悄当精确基线。

修复要求：task 只要存在 legacy `file_backup` 与新 change 混合即 typed Unrestorable/fail closed，或在新 write 副作用前禁止为该 task 建立新记录；不得消费脱敏 legacy previous。加入升级后 resume/write 的真实夹具。

### [P1] change path 没有重新绑定原工具调用参数

位置：`src/engine.rs:1181`～`1201`，`src/store.rs:771`～`831`。

prepare/restore 只比较 change row 与 approval 中复制的 binding digest、workspace/scopes；restore 没有从 task 原始 tool call 解析 args、重算 approval binding，也没有证明 `record.path`/after digest 对应原 `write_file` path/content。若 change row 的 path/path_key 被损坏但 binding_digest 未变，恢复可作用于另一条 scope 内路径，而不是 typed Corrupt。

修复要求：restore 从 durable task message 找到 tool_call_id，严格解析 name/args，重算 binding digest并规范化 path，要求与 record.path/path_key/after_digest 对应；任何差异 typed Corrupt/Conflict、零文件写。加入 path、content、binding、tool_call 的独立伪造反例。

### [P1] Workspace HTTP 内部错误可能泄露 SQL/路径

位置：`src/server.rs:935`～`955`。

`workspace_change_error` 对 typed Corrupt/Internal 和所有未知 anyhow 直接把原 error 交给 `ApiError::with_code`。SQLite、IO、DPAPI 或路径错误的原 message 可能进入 JSON `message/error`，泄露 SQL、绝对路径或敏感上下文。partial receipt 特例使用 typed 固定文案较安全，但普通 500 不安全。

修复要求：typed 4xx 使用固定业务文案；500 按 busy/corrupt/internal 映射固定安全文案和 retryable，原 error 仅内部使用。HTTP 反例注入含 SQL、绝对路径和秘密样式的 DB/IO 错误，断言 body/header 不泄露。

### [P2] 两个新路由没有 persisted-ID 校验

位置：`src/server.rs:904`、`src/server.rs:916`。

`GET changes` 和 `POST restore` 只调用通用 `object_id`，未像 approval 路由一样调用 `secrets::validate_persisted_id`。控制字符、超长和敏感样式 ID 会进入 Store 查询/错误链，与契约“persisted id 校验”不符。

修复要求：新增 workspace task-id helper，先执行共享校验，再访问 Engine；非法值固定 400、零 Store/文件副作用且不回显原 ID。路径 segment 编码规则保持 server 既有行为。

## BP01～BP07 复核

| 编号 | 固定候选结论 |
| --- | --- |
| BP01 | 已关闭：prepare/claim/finish 提交后显式 drop DB guard，再调用会重锁的方法。 |
| BP02 | 主体关闭：start/start_idempotent/resume/write/restore 共用生产 Engine gate，并检查同 workspace 活动任务；不宣称跨 Engine/进程互斥。并发屏障证据仍未完成。 |
| BP03 | 已关闭：restore 在 claim/任何文件写之前解密并校验全部 before。 |
| BP04 | 已关闭主体：逐路径 action 前重新 resolve 全父链、比较目标和 current digest；真实 reparse/TOCTOU 证据未完成。 |
| BP05 | 部分关闭：首次 before/最新 finished after 与 later unknown 有模型，但 failed 污染和 partial 后 restorable 矛盾仍在。 |
| BP06 | 部分关闭：比较当前 settings workspace、任务 scopes、approval workspace/binding；未从原 tool args 重算绑定并验证 change path/content。 |
| BP07 | 代码关闭：schema 8 对表 DDL 做规范化全等，对 index unique/partial/列和谓词做核验，并执行 foreign_key_check；现有伪索引测试有限，W11 完整夹具仍待 B 补齐。 |

工具 description 已改为受保护恢复数据/安全事件元数据。`scope_path` 已拒绝尾点/尾空格、控制字符和 Windows 设备名，existing path 通过 canonical identity，未在本候选发现独立可复现的路径别名绕过；仍需 W05 的 Windows 实测。

## B 已承认的证据未完成

这些项目目前没有足够夹具或门禁日志，候选本身也尚未宣称完成：

- W02 连续成功写、失败准备不计成功、首次 before 恢复。
- W04 跨 task 写入冲突和新文件后改不删除。
- W06 多文件预检零写、执行期故障 partial/unknown 和逐路径事实。
- W07/W08 真实子进程停止窗口与重启 unknown，不以 drop Arc 代替。
- W09 恢复幂等、并发 gate、restore 后 resume/write 封存。
- W10 DPAPI 正文 roundtrip、事件/HTTP/SSE 全面泄露扫描、过大/不支持输入零副作用。
- W12 legacy backup 及混合 legacy/new 不可恢复。
- W13 真实 HTTP DTO、persisted ID、鉴权/安全头、partial receipt 和安全错误。
- W14 五标准门禁及完整回归。现有 4/4、2/2、check/node 只能作为中间开发证据。

## 下一步

B 应在同分支新增提交修复上述实现问题并完成其已承认的 W/门禁证据。经理固定新 SHA 后，A 只审相对 `dd57adf` 的差异、故障夹具和门禁结果。C 候选复核另见第1.3报告，本报告不混合 C 结论。
