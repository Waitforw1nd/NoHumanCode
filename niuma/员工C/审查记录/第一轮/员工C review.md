> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 C 第 1 轮审查：HTTP 与 SSE 传输契约

日期：2026-09-23  
审查人：项目负责人  
结论：**CHANGES_REQUIRED，完成下列修复后提交第 1.1 轮。**

依据：[员工 C 任务单](../../任务/第一轮/员工C提示词.md)、[员工 C 第 1 轮报告](<../../提交报告/第一轮/员工C（HTTP与SSE传输契约 第1轮报告）.md>)、[项目开发守则](../../../../项目开发守则.md)。  
附件：[复现说明、源码与实际日志](员工C审查附件/README.md)。

## 1. 结论与已确认成果

C 的 **11 项 HTTP 测试实际通过**，fmt、Clippy、WASM 检查也通过。Host/Origin/cross-site/token 的判断保持，拒绝响应补齐结构化 JSON 和安全头；JSON 提取错误及通常的游标、读取、SSE 续传路径已有实现。没有挂载 B 的新 Turn 写路由。

不能放行的原因是四类已复现的传输问题：编码后的游标参数名绕过校验，部分存储一致性损坏误报 400，路径提取错误返回纯文本，损坏事件名使 SSE 编帧 panic。此外 H7～H10 的夹具未证明报告所述的完整场景，需要校正证据。

负责人补充 7 项隔离探针：**2 通过、5 失败**。5 条反例对应 4 类实现问题，游标问题有两个独立反例；不能说成“C 原来的 11 项有 5 项失败”。两项通过结果是：

- health 的 schema 读取失败确实返回安全的 500/internal，不再伪装为 ok/schema0；负责人已补足报告承认缺失的 HTTP 证据。
- 同一 SSE 连接先成功收到事件，再使后续事件 JSON 损坏，确实只发一条 error、保留已发游标并结束。该路径不是实现缺陷；员工原 H10 没有测到它，需把有效场景纳入正式回归。

本轮只审查、复现和写文档，没有修改共享生产代码或正式测试。C 修复继续限于 `server.rs`、必要的 `src/server/` 辅助模块、`tests/http_contract.rs` 和自己的文档；不替 B 修领域、Store 或 B 测试。

## 2. 审查基线与测试边界

HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`。实际对象包含 A 已验收未提交修复、C 交付和 B 正在返工的工作区，不能只检出 HEAD 复验。

源码隔离副本：`./.local/temp/fufu-c-review-22cad04b86`。最终复核时 `server.rs`、`http_contract.rs` 与快照 SHA256 一致；B 的 `session_turns.rs` 已继续变化。附件保存被审查 server、探针、哈希与日志，以避免混淆并行版本。

| 检查 | 实际结果 | 限制/证据 |
| --- | --- | --- |
| 独立 `cargo test --locked --test http_contract -- --test-threads=1` | 11 通过、0 失败，退出 0 | `review-http-contract.log` |
| 原有 fmt / Clippy / WASM | 均退出 0 | `review-gates.json` 及对应日志；审查探针未纳入这些门禁 |
| 首次全量检查，共用编译缓存 | 未完成，退出 101 | `session_turns` 的输出 EXE 无法打开，LNK1104；随后改用独立 target，没有删除/终止别人的构建产物或进程 |
| 独立 target 全量检查 | 未完成，不能声称全绿 | 已完成 43 lib、4 adversarial、9 final_acceptance、11 http_contract、10 runtime；live 1 忽略。B 的 12 个 session_turns 中 11 项已通过，剩余 T3 超过 60 秒未结束，负责人停止了自己的隔离测试进程；后续 protocol 等未走完 |
| 7 项审查探针 | 2 通过、5 失败，退出 101 | `review-probes.log`，对应下文 |

全量阻塞的具体原因在隔离版本的 B 测试：`#[tokio::test]` 默认单线程，先 `tokio::spawn` 再在当前线程执行 `ready_rx.recv()`，被等待任务没有机会运行；worker 内也用了 `go_rx.recv()`。位置见附件 `b-intermediate-session_turns.rs:423`、`:433`。这是 **B 返工中快照的问题，不计为 C 缺陷，也不代表 B 最新版本仍未修复**。后续 B 应用异步 channel/Notify 或合适的同步工作线程完成确定性并发测试；C 不改此文件。

本次使用真实 PowerShell **7.6.5**，路径为：

`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`

所以 C 报告中的“本机没有 pwsh”应收窄为当时进程环境找不到该命令。负责人实测既有 runtime 的 PowerShell 用例通过。报告如实保留了失败，没有把全量失败冒充成功，这一点成立；环境问题和本轮实现问题分别记录。

## 3. 必须修复的问题与具体思路

### C-R1 · P2：编码后的 after 参数名绕过校验，非法输入变为从 0 重放

**位置：** [server.rs:687](../../../../NoManCode/rust-app/src/server.rs#L687)。对应 C-04、H5/H9。

`query_cursor` 在 URL 解码前比较 `name == "after"`，只解码参数值。因此同一个参数的编码写法被当成未知参数忽略。

两个实际 HTTP 反例：

```text
Session SSE: ?after=0&%61fter=1  → 200，未拒绝重复 after
Run SSE:     ?%61fter=-1         → 200，并重放 seq=1；应在开流前 400
```

`%61fter` 解码后就是 `after`。第二个请求不仅漏了一个错误码，还把非法游标静默改为默认 0。

**修改顺序：** 按原 query 分割键值对 → 对参数名进行严格 URL 解码 → 按解码后的名字识别 after 并计数 → 严格解码/验证其值 → 再验证 Last-Event-ID → 两者合法才执行 header 优先。两个 SSE 路由共用同一解析器。

保持现有“不重复、不回退、ASCII 数字和 i64 上界”的契约；未知参数仍按已决定的兼容策略处理。解码实现按字节操作，检查百分号后两字节是否十六进制，不以未经检查的 UTF-8 字符串下标切片。无需新增 Cargo 依赖。

**闭合验收：** 两种路由都覆盖编码参数名、明文与编码重复、两个编码重复、编码后的非法值、非法 query 配合法 header。非法情况均在开流前返回 400/request_failed JSON；合法编码写法保持数值语义，不重置游标。

### C-R2 · P2：非 rusqlite 类型的存储一致性故障被错分为请求错误

**位置：** [server.rs:215](../../../../NoManCode/rust-app/src/server.rs#L215)，尤其第 250 行的兜底。对应 C-02、H2。

`read_error` 只把 rusqlite/serde_json 类型识别为内部读取故障，其余错误回落为 400。实际 Repository 的一致性校验也会用普通 `anyhow::ensure!` 报错；这不是用户输入校验。

复现：正常建立 Run，仅在临时库中把 `tasks.value` 的 JSON 内 `id` 改为另一值，保持 `tasks.id/run_id` 列与 Run 不变。请求该已存在 Run：

```text
GET /api/runs/{existing_run}
实际：400 / request_failed
期望：500 / internal / retryable=false
```

底层已经识别了身份损坏；server 误分类。该底层校验在 C 前基线已经存在，不归因于 B 后来的返工。

**具体修法：** 在已完成参数解析、明确执行 Store 读取的边界使用严格读取分类：确定 QueryReturnedNoRows → 404；明确 busy/locked → 500 且可重试；其他该读取操作的错误 → 500 且不可重试，输出通用安全消息。

不能直接把所有 `ApiError::from(anyhow)` 改为 500。将“存储查询失败”与后续“缺少设置/找不到配置路由/业务参数校验”的处理分开；例如先对 `store.settings()` 分类，再单独处理 Option 和 route 查找。业务入口维持现有分类。这可以在 C 的 server 文件完成，不需要给 B 的 Repository 加中文字符串匹配或新 SQL。

**闭合验收：** 保留真实不存在 404；并测无效 JSON/枚举、合法 JSON 但关系身份不一致两类损坏均为 500/internal，安全头保持，消息不含原始值、SQL、内部路径，数据库和 Provider 调用数不变。

### C-R3 · P2：Path 提取失败绕过统一错误体

**位置：** [server.rs:506](../../../../NoManCode/rust-app/src/server.rs#L506) 及同类 `Path<String>` handler。对应 C-02 输入错误契约。

真实请求 `GET /api/sessions/%FF` 返回 400，但 Content-Type 为 `text/plain; charset=utf-8`，没有 code/message/retryable/error。安全头仍有，问题是 Axum 在进入 handler 前完成路径提取并直接返回默认拒绝；`ContractJson` 无法覆盖它。

**具体修法：** 增加 server 层路径提取适配器，或让 handler 接收 `Result<Path<String>, PathRejection>` 后统一转换。非法参数返回安全的 400/request_failed JSON，不直接回显框架错误原文。仅适配已有 API 参数，不扩大到未知路由、静态资源或 HTTP 解析器根本未接受的畸形报文。

**闭合验收：** Session、Turn、Run 查询及两种 SSE 的非法 UTF-8 编码路径统一 JSON；SSE 必须在开流前拒绝。合法不存在 ID 仍是 404，正常读取形状不变。

### C-R4 · P2：SSE 编帧的 panic 不在现有错误分支内

**位置：** [server.rs:786](../../../../NoManCode/rust-app/src/server.rs#L786)，尤其 `.event(&event.kind)`。对应 C-06、H10。

`serde_json::to_string(&event)` 成功，不代表后面的 SSE 帧构造安全。Axum 的 `.event()` 不允许 CR/LF，遇到后会 panic。正常 Store 写入已有 kind 校验，但读取损坏的持久化行时不保证它仍合法。

实际复现：在同一 HTTP 流先收到正常事件 seq=1 → 通过合法 delta 写入后续事件 → 仅在临时库把后续 kind 改为 `bad\nkind` → 继续读取同一 Response。

```text
Axum panic: SSE field value cannot contain newlines or carriage returns
客户端：UnexpectedEof；没有 event:error，也没有 after
```

这个潜在边界在旧实现也存在，但 C 本轮承诺收口流内读取/编码故障，仍未处理它。它不是“正常 serde Event 必然能触发 JSON 编码失败”，两者必须区分。

**具体修法：** 在调用 SSE builder 前验证帧字段。可以复用现有公开 `domain::validate_event_kind`，至少必须防止 CR/LF；不修改 domain 的契约。验证失败按存储数据损坏发一次安全 error，`retryable=false`、after 保持最近已发游标，随即结束外层流。不要修改原事件、给它分配新 seq，或以 catch-all panic 掩盖非法字段。成功完成所有编码/构帧步骤后才推进游标。

**闭合验收：** 同一流先收到一条正常事件，分别注入后续 data JSON 损坏和 kind CR/LF 损坏；都必须为一次 error → EOF，after 等于正常事件 seq，无错误帧 id，无新增领域记录/模型调用。若尚未发正常帧，则 after 保持请求有效游标。

### C-R5 · P2：H7～H10 的测试前置条件不足，报告需纠正

| 条目与位置 | 当前实际证据 | 第 1.1 轮应补的有效场景 |
| --- | --- | --- |
| H7，[http_contract.rs:792](../../../../NoManCode/rust-app/tests/http_contract.rs#L792) | 全是 completed 夹具，`is_busy()` 前后均 false；证明了重连查询，未证明正在执行的任务不会被断线取消 | 用本机 mock 的 Notify/gate 保持真实任务 running；连接、消费、断开后确认仍运行，释放 mock 后正常完成，Provider 仅首次执行一次 |
| H8，[http_contract.rs:873](../../../../NoManCode/rust-app/tests/http_contract.rs#L873) | 实际插入顺序 Left1、Left2、Right1，Left 内无 seq 间隔，显示名也不同 | 构造 A1、B1、A2，两个 Session 使用相同显示名/标题但不同 ID；断言 A 的两个 seq 中间确有 B，并且两条流各自只返回本范围事件 |
| H9，[http_contract.rs:941](../../../../NoManCode/rust-app/tests/http_contract.rs#L941) | `future = max_seq`，只验证当前位置等待。H5 有更大游标的短时无重放，未验证事件逐步跨越未来游标 | 设置 `future = max_seq + N`，先追加仍 ≤ future 的事件并确认不返回；再产生 > future 的事件才收到。也保留普通空闲后续传 |
| H10，[http_contract.rs:1020](../../../../NoManCode/rust-app/tests/http_contract.rs#L1020) | 正常事件只通过 Store 读取，HTTP 请求之前已破坏下一行，再带 after 开流；只能证明“未发出事件时保留请求 after” | 在同一 HTTP 流成功读到正常帧后再故障注入，并断言 error、EOF 和最后已发游标。负责人已用 data JSON 损坏证明该路径可通过；再补 C-R4 编帧边界 |

当前 `read_frames` 返回时会 drop Response，不适合上述分阶段验证。补一个保留 Response 与剩余字节缓冲的增量读帧器，使用明确 timeout/通知同步，不用无限收集长流或任意长 sleep。

H10 报告的“先读到正常事件，再破坏后续事件 JSON”若指 HTTP 验收，与实际测试不符；第一轮报告保留历史，第 1.1 轮明确纠正，不能继续照抄。负责人补测通过不自动等于员工原测试已覆盖。

## 4. 已补足证据，不列为新缺陷

### Health 故障夹具

无需新增 Store 故障注入 API。保持正常已打开的 Store，在另一条测试专属 SQLite 连接执行 `PRAGMA user_version=-1`，`schema_version()` 的无符号数转换会失败。随后真实 HTTP `/api/health` 返回 500/internal/retryable=false，且没有 ok/schema_version 成功字段。

该探针已通过，附件可直接参考。C 可将它加入正式测试，并更新契约中“未找到稳定 HTTP 夹具”的说明；不要改 schema 实现或生产数据库。

### 环境与并行集成

C 不需要因报告的 PowerShell 环境失败而修改 workspace 或构建脚本。第 1.1 轮记录真实宿主路径/版本；可使用负责人已验证的 PS7 路径。B 的中间态测试阻塞单独记录，待 B 修复后安排全量集成，不把 C 独立 11 项通过称作整个仓库通过。

## 5. 返工顺序与验收边界

1. 修 C-R1～C-R4，新增能失败于当前版本的针对性 HTTP 反例。
2. 按 C-R5 修正夹具与证据，吸收 health 和真实开流后故障的已通过复现。
3. 更新自己的 HTTP-SSE 契约与第 1.1 轮报告，逐项保留 C-R1～C-R5 编号，分别列出实现、测试与未完成项。
4. 跑独立 HTTP 测试、fmt、Clippy、WASM 及补丁检查；全量结果如受 B 中间状态影响仍分开记录。不能删除/忽略 B 测试来声称全量通过。

无需重做现有成功接口、安全判断、事件 envelope 或幂等层。保持现有数据结构和权限，不加依赖，不接新 Turn HTTP 写入口。所有问题均可在 C 的现有文件所有权内处理。

## 6. 可直接给员工 C 的返工提示词

```text
你是员工 C，继续 HTTP/SSE 传输契约任务，工作目录 ./。

完整阅读：
./niuma\员工C\审查记录\第一轮\员工C review.md
./niuma\员工C\审查记录\第一轮\员工C审查附件\README.md
并沿用项目开发守则和原 C 任务单。

第一轮结论 CHANGES_REQUIRED。直接实现并测试 C-R1～C-R5：
- URL 解码参数名后识别/计数 after，拒绝编码写法绕过校验或重复。
- 纯存储读取的一致性损坏返回 500/internal，同时保留业务校验的 400。
- 将 Path 提取错误纳入安全 JSON 错误契约。
- SSE 构帧前校验 kind，损坏时一次 error、保留 after、结束流，不 panic。
- 补真实运行中断线、A1/B1/A2 交错、未来游标跨越、同流先成功后故障测试；
  将负责人已通过的 health 故障和 JSON 读取故障场景纳入正式回归，纠正报告证据。

review 已给出具体入口、复现和修法。只改 C 拥有的 server/独立 HTTP 测试和文档。
不要修改 B 的 Engine/Store/Repository/domain 或 session_turns 测试，不改 schema/protocol，
不挂载新 Turn 写接口，不全仓自动格式化，不自行宣布审查通过。
全量检查受 B 返工中状态影响时单独记录，不能用跳过测试制造全绿。

报告写到：
./niuma\员工C\提交报告\第一轮\员工C（HTTP与SSE传输契约 第1.1轮报告）.md

逐项保留 C-R1～C-R5，附实现位置、真实测试和副作用断言、实际命令/退出码、
真实 PowerShell 路径与版本、未执行项。保留第一轮报告，等待负责人复审。
```
