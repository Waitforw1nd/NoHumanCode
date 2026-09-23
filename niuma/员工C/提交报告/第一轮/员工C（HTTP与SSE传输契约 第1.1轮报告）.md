> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 C：HTTP 与 SSE 传输契约 第 1.1 轮报告

日期：2026-09-23。对应审查 `CHANGES_REQUIRED`。本报告不宣布审查通过。第 1 轮报告保留。

## 1. 基线

- HEAD：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`
- 本轮仍只改 `rust-app/src/server.rs`、`rust-app/tests/http_contract.rs`、自己的契约文档和本报告。
- 没有修改 B 的 Engine、Store、Repository、domain、`session_turns.rs`，没有改 schema/protocol，没有挂载新 Turn 写接口，没有全仓自动格式化。

第 1 轮报告说“本机没有 pwsh”应收窄为当时进程环境找不到该命令。本轮确认真实 PowerShell 7.6.5 位于：

`./.local/user-profile/.cache\codex-runtimes\codex-primary-runtime\dependencies\native\powershell\pwsh.exe`

## 2. C-R1～C-R5

| 编号 | 实现 | 测试 | 状态 |
| --- | --- | --- | --- |
| C-R1 | `query_cursor` 先按字节严格 URL 解码参数名，再识别和计数 `after`。`%61fter` 与明文重复会被拒绝；非法编码值不再退回 0。解码后仍含 `%` 的名字拒绝 | H5：明文/编码重复、两个编码重复、编码非法值、非法 query 加合法 header；Run SSE 的 `%61fter=-1` 在开流前 400 | 已实现且验证 |
| C-R2 | 已进入 Store 读取后，除缺行和幂等冲突外都返回安全 500。settings 读取与随后的“缺少设置/路由不存在”分开 | H2：真实不存在仍 404；非法 kind 与 `tasks.value` 内 id 不一致都是 500/internal/retryable false，消息不含坏值 | 已实现且验证 |
| C-R3 | handler 接收 `Result<Path<String>, PathRejection>`，非法路径转为 400/request_failed JSON | H4：Session、Turn、Run 和两种 SSE 的 `%FF` 都是 JSON 400，开流前拒绝 | 已实现且验证 |
| C-R4 | `sse_frame` 在 builder 前调用现有 `validate_event_kind`，并拒绝 id/kind 的 CR/LF。失败发一次 error，不推进游标，不 panic | H10：同一 HTTP 流先收到正常帧，再分别注入 data JSON 损坏和 `bad\nkind`；一次 error 后 EOF，after 等于已发 seq，无错误帧 id | 已实现且验证 |
| C-R5 | 校正 H7～H10 夹具，并纳入 health 故障 | 见下表 | 已实现且验证 |

C-R5 的校正：

- H7：本机 mock 用 `Notify` 保持真实任务 running。消费一帧后断开，任务仍 busy；释放 mock 后完成，Provider 调用次数保持 1。重连只收到更大的 seq。
- H8：插入顺序是 A1、B1、A2。两个 Session 使用相同标题和显示名、不同 ID。A 的两个 seq 中间确有 B，两条流各自只返回本范围。
- H9：未来游标是 `max_seq + 1`。先追加仍不大于该游标的事件，查询结果为空；再产生更大的事件才收到。
- H10：第 1 轮只在开流前破坏下一行，不能证明 HTTP 已成功收到正常帧。第 1.1 轮改为同一 Response 上先读正常帧再故障注入。第 1 轮报告的那句证据不再沿用。

health：另一条测试连接执行 `PRAGMA user_version=-1` 后，真实 `GET /api/health` 返回 500/internal/retryable false，没有 `ok` 或 `schema_version`。

## 3. 公共契约影响

- 成功读取形状、事件 envelope、安全判断和既有幂等 409 保持。
- 编码后的 `after` 不再被忽略。未知且完整解码后的其他参数仍不拒绝。
- 存储读取的一致性损坏从 400 改为 500。业务参数校验仍是 400。
- 非法路径从纯文本 400 改为结构化 JSON。
- Schema、迁移、权限和凭据算法无变化。错误消息继续经过既有 scrub。

## 4. 实际命令

| 命令 | 结果 | 退出码 |
| --- | --- | --- |
| `cargo test --locked --test http_contract -- --test-threads=1` | 11 通过，0 失败 | 0 |
| `powershell.exe -ExecutionPolicy Bypass -File build.ps1 -Action fmt` | 通过 | 0 |
| 同上 `-Action clippy` | 通过；仅有既有 `proc-macro-error2` 提示 | 0 |
| 同上 `-Action wasm-check` | 通过；同样只有既有提示 | 0 |
| `git diff --check` 针对本轮拥有文件 | 通过 | 0 |

本机进程环境仍可能找不到 `pwsh`。上表脚本命令使用 Windows PowerShell 调用项目脚本；真实 PS7 路径和版本已记录。没有重跑全量测试：审查已说明 B 的中间态 `session_turns` 会阻塞，本轮不改该文件，也不把独立 11 项称为整个仓库通过。未执行付费 live 测试。

## 5. 未完成

- 新 Turn HTTP 写入口仍未挂载，等 B 契约验收后的联合任务。
- B 的领域应用错误仍没有统一 HTTP 映射。本轮只收口已进入存储读取的错误。
- 全量集成仍待 B 的并发测试修复后复验。
