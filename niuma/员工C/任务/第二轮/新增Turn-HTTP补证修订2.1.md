# C-R2-01 / NEXT-01 修订2.1：Turn HTTP 验收补证

日期：2026-09-23。项目 NoHumanCode；仓库根 `./`；源码 `NoManCode/`；Rust 目录 `NoManCode/rust-app`。

## 来源与状态

这是经理 [C review 2.0](../../审查记录/第二轮/员工C%20review%202.0.md) 的补证修订，交用户分发后由原 C 执行者继续。本文件只是待分发任务，不代表已经开工。原执行标识 `C-R2-01-20260923-0701` 保持；同一任务仍只有 C 一个当前执行者。

## 基线、所有权与 Git

- 共享工作树 `./` / `main`；HEAD `8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5` 加当前有效未提交成果，不能 reset、clean、覆盖或从旧 HEAD 重建。
- 当前 C 候选：`NoManCode/rust-app/src/server.rs` SHA-256 `d89f035c0fdb8ca8b3e316e73e2d8f80a3e134d535e6a66e743d20f1f9959667`；`NoManCode/rust-app/tests/turn_http.rs` SHA-256 `430a6f9e8792d95f0e9d95b02736d4a71a7c6242a543024167f4737638f629c8`。开工先重新核对并把新哈希写报告。
- C 是 `server.rs` 与 `tests/turn_http.rs` 的唯一源码写入人；可写自己的报告、证据和身份。不得改 Engine、Store、Repository、domain、secrets、protocol、schema、Cargo/锁文件、构建脚本、UI、D 的三处源码或旧正式测试。
- 经理是唯一 Git 暂存、提交、分支和整合执行人；C 不提交 Git。报告、Git 提交、验收、合并和发布分开记录。

## 目标

只补强 N07、N09、N10、N12、N14 的有效前置和副作用证据，修正 N11 的后台调度等待，并在报告中准确标注 N05/N06 客户端构造边界。保持现有 POST `/api/sessions/{id}/turns` 实现、Engine replay-first 事务和第一轮 HTTP/SSE 契约，不扩大产品范围。

## 必做修订与验收断言

1. **N07 累计消息阈值**：根据当前 predecessor 的实际 `messages` JSON 序列化字节数动态构造一个 `<= 1_500_000` 的成功边界和一个 `> 1_500_000` 的超限请求；在发请求前记录长度并断言关系。超限响应必须是 `400/request_failed/false`，11 表逐单元格不变、Provider 调用差值为 0；边界成功可回放。不能继续用固定 `1_460_000` 而不证明实际长度。
2. **N09 结构/归属/工具历史**：foreign agent、foreign turn、team、双 Agent 的完整 11 表快照与 Provider 调用计数必须在请求前建立。工具历史的 `tools=true`、`role=tool`、非空/错误类型 `tool_calls` 必须使用独立 fresh fixture 或每例恢复同一合法基线后重新注入，不能让前一例污染后一例；空数组正例需在清洁 `tools=false`、首条 user 前置下成功。每一例断言 `400/request_failed/false`、无写入、无调用。
3. **N10 前序状态**：stale、running、behind 每个请求都要有请求前快照和调用计数；running 保持真实 `hold:` partial 前置，释放只在所有断言后进行。每例断言 `409/conflict/false`、无对象/事件/幂等写入和 Provider 调用。
4. **N12 异 key race**：barrier 前记录 11 表快照和调用计数；两请求完成后取 after 快照，断言恰一胜者新增一组 Turn/TurnTask/Task/idempotency/idempotency_record/event，败者 409 无写入、无调用；失败 key 后续重试单独记录新增，不能掩盖竞争批次。
5. **N14 配置故障**：missing route、missing secret、不可达 URL 三个新 key 请求分别记录前快照/调用计数，断言 `500/internal/false`、无写入、无 Provider 调用。另用已有成功 key 验证配置删除后仍 `200/replayed=true`，恢复配置后新 key 才能成功。
6. **N11 调度等待**：同 key 和双 Engine 竞争在断言 `calls_for` 前用现有 `wait_delta` 或等价有界等待观察 Provider 调用，不能只在 HTTP 响应返回后立即读取后台计数。
7. N05 `%FF` 和 N06 非 ASCII header 若仍只能在客户端构造阶段失败，报告必须写成客户端边界证据，不得声称服务端返回 400；N16 interrupted 可一并补请求前后快照。

## 验证与交付

- 先运行针对性的 `turn_http` N07/N09/N10/N11/N12/N14，再按当前环境实际可用命令运行 workspace test、fmt、clippy、wasm-check；未运行项目不得写“通过”。付费 live 保持 ignored，不读取或新增凭据。
- 报告目标：`niuma/员工C/提交报告/第二轮/员工C（新增Turn-HTTP接入 第2.1轮补证报告）.md`；证据放同目录 `NEXT-01-2.1证据/`。逐项列测试名、前置长度/快照哈希、退出码、Provider 调用差值、未执行项、最终 SHA-256 与 Git 状态。
- 交回后经理重新 review；在 C review 通过和 C/D 固定候选串行联合门禁前，NEXT-01 不得写成已验收。若同一问题再次未闭合，下一 review 必须给出新的具体入口、算法顺序、有效反例与断言。
