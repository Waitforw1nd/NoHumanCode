# 员工 C：新增 Turn HTTP 接入 第 2.1 轮补证报告

日期：2026-09-23。任务：C-R2-01 / NEXT-01 修订2.1；执行标识 `C-R2-01-20260923-0701`（沿用）。对应 [员工 C review 2.0](../../审查记录/第二轮/员工C%20review%202.0.md) 的 CHANGES_REQUIRED，补证要求见 [修订2.1](../../任务/第二轮/新增Turn-HTTP补证修订2.1.md)。本轮只改 `tests/turn_http.rs` 与报告/证据/身份；`server.rs` 未再修改（哈希与 2.0 轮相同）。本报告不宣布验收通过；提交不等于 Git 提交、验收、合并或发布。

## 1. 基线核对

开工时重新核对：HEAD `8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`、main、共享工作树 `./`；`server.rs` sha256 `d89f035c…9667`、`turn_http.rs` 当时 sha256 `430a6f9e…629c8`，与 review 2.0 记录一致；无修订3、无同任务其他执行者写入。D 的 `plugin_catalog.rs`/`lib.rs` 导出与其测试未动。

## 2. 逐项补证（review 2.0 → 改动 → 断言）

证据文件在 `NEXT-01-2.1证据/`：`test-workspace.txt`（最终版全量）、`test-turn_http-nocapture.txt`（含 n07 长度输出）、`build-actions.txt`、`哈希与git状态.txt`。

| Review 条目 | 测试/位置 | 本轮改动 | 观察与断言 | 状态 |
| --- | --- | --- | --- | --- |
| P1·N07 阈值 | `n07_message_bounds_and_body_limit` | 删除固定 `1_460_000` 注入；改为读取前序 task 的 `value.messages`，按 Engine 判定（`engine.rs:400` 对 `to_vec(messages+新user)` 的长度判断）动态计算：先求 `fixed`（零填充候选长），再取 `over_pad=1_500_001-fixed`、`boundary_pad=1_500_000-fixed` 分别注入 | nocapture 实测输出：`fixed=200988, over_pad=1299013, over_len=1500001, boundary_pad=1299012, boundary_len=1500000`。超限请求前断言 `candidate_len>1_500_000` → 400/request_failed/false + 11 表逐单元格不变 + `calls` 差值 0；随后把 tasks.value 精确恢复为注入前文本，注入 boundary 填充，断言 `<=1_500_000` → 201，同 key 回放 200 | 通过 |
| P1·N09 前置快照 | `n09_session_shape_and_404` | 三个 404 与 foreign_agent/foreign_turn/team/双 Agent 全部改为「请求前 `snapshot`+`calls` → 发请求 → 断言 → `assert_untouched`+`calls` 相等」。先前排版把快照放在 send 之后，已纠正 | 每个负例 400/request_failed/false（404 例为 not_found），前后 11 表逐单元格相等、Provider 差值 0 | 通过 |
| P1·N09 故障隔离 | 同上 mutations 段 | 注入前保存 predecessor task 的 `value` 原文；每例先 `UPDATE tasks SET value=原文` 恢复同一合法基线，再注入单一故障（tools=true / messages[0].role=tool / messages[1].tool_calls 非空数组 / tool_calls 为字符串） | 四例各自 400/request_failed/false、快照不变、调用差值 0；正例在恢复后的清洁基线（tools=false、messages[0]=user、无残留 tool_calls）上仅注入 `tool_calls=[]` → 201 | 通过 |
| P1·N10 前置证据 | `n10_predecessor_states` | stale/running/behind 三分支补请求前 `snapshot`+`calls_before`；cancelled/interrupted 分支补 `calls_before`。running 保持 `hold:`+`wait_delta(partial-busy)` 真忙态，`release.notify_waiters()` 仍在全部断言之后 | 五例均 409/conflict/false、注入后快照不变、调用差值 0 | 通过 |
| P1·N12 竞争记账 | `n12_different_key_race` | barrier 前 `snapshot`+`calls`+`max_seq`；两响应后按 201/409 归类并取胜者 turn/task；`wait_delta`+`wait_task` 落定后取 after 快照 | 恰一胜者：`turns/turn_tasks/tasks/idempotency/idempotency_records` 各 +1 且旧行前缀不变；`seq>base` 的新事件数 == 归属胜者 turn_id+task_id 的事件数（>0）；胜者消息 `calls_for`=1、败者 0、总差值 +1。失败 key 以最新已完成 turn 重试：独立前后快照各 +1 组、事件归属新 turn、该消息调用 1 次——不与竞争批次混记 | 通过 |
| P1·N14 配置故障 | `n14_config_faults_500_and_replay_first` | missing route / missing secret / 不可达 URL 三个新 key 请求各自独立 `snapshot`+`calls_before` → 500/internal/false → `assert_untouched`+差值 0 → 恢复 | 三例均 500/internal/false、11 表不变、Provider 0 调用；route 删除期间已提交同 key 仍 200/replayed=true/同 turn（回放先于配置检查）；全部恢复后新 key 201 | 通过 |
| P1·N11 调度等待 | `n11_same_key_concurrency` | 两段并发（单 Engine、双 Engine+Barrier）在断言 `calls_for` 前先 `wait_delta(db, legacy, "reply")` 观察持久化 delta、再 `wait_task` 等 completed，最后读计数 | 两例均 1×201+1×200、同 Turn/Task、五表各 +1、queued 事件恰一条；`calls_for`=1 在任务落定后断言，非响应后立即读 | 通过 |
| P2·N16 | `n16_real_restart_partial_replay_resume` | recover 后、重开 HTTP 后取 `replay_before`/`replay_calls`；回放与 `after-interrupt` 拒绝请求各自前后快照与调用差值 | 回放 200/replayed=true/同 turn（status=interrupted）、11 表不变、Provider 0 增；中断前序追加 409/conflict/false、无写入无调用；另断言重启期间 `replay_calls==calls_after`（recover 本身不调 Provider） | 通过 |
| P2·N05 `%FF`/`%20` | `n05_body_and_id_validation` | 原双分支 match 改为构造+发送必须成功：`build().unwrap_or_else(panic)` + `execute().unwrap()` + 断言服务端 400。实测通过说明两路径均到达服务端（`%FF` 在 `Path<String>` percent 解码成非 UTF-8 → `path_rejection`→400；`%20` 解为空格 → `validate_persisted_id`→400） | 服务端证据成立，非客户端构造边界；若未来客户端层拒绝会以 panic 显形 | 通过（服务端 400 已实证） |
| P2·N06 非 ASCII | `n06_idempotency_header` | 同上收紧：`HeaderValue::from_bytes` 必须成功（http 1.5 `is_valid` 允许 obs-text 0x80–0xFF），请求必须发出，断言服务端 400 | `kéy`（UTF-8 `0xC3 0xA9`）与裸 `0x80` 均真实到达服务端，被 `required_idempotency_key` 的 `to_str` 拒绝 → 400/request_failed/false。属服务端证据 | 通过（服务端 400 已实证） |
| P2·N03 转义 | `n03_replay_variants` | raw body 的 message 实际写入 `\u0068old:run`（`h` 的等价 `\u` 转义）+ 字段换序；注释同步为真实内容 | 回放 200/replayed=true/同 turn，证明哈希按解析后语义计算 | 通过 |

## 3. 实际执行命令与结果

环境沿用 2.0 轮记录：Git 内嵌 pwsh shim（`-NoProfile -ExecutionPolicy Bypass`）、`CARGO_BUILD_JOBS=1`、build.ps1 自举 MSVC/SDK。

| 命令 | 结果 | 退出码 |
| --- | --- | --- |
| `cargo test --locked --test turn_http`（等效 MSVC env，定向先跑） | 17/17 通过；n09 初次补快照时暴露 team 分支沿用了 foreign_agent 的旧 `calls_before`（spawn_chat 本身+1 调用），已为 foreign_turn/team 各自补独立计数后通过 | 0 |
| `cargo test --locked --test turn_http -- --nocapture` | 17/17；含 `n07 cumulative: fixed=200988 over_pad=1299013 over_len=1500001 boundary_pad=1299012 boundary_len=1500000` 实测行 | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action test`（最终格式化版） | lib 53、adversarial 4、final_acceptance 9、http_contract 11、live 1 忽略、plugin_catalog 17、runtime 10、session_turns 17、turn_http 17、protocol 5；共 143 通过 0 失败 1 忽略 | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action fmt` | 首次报本轮新代码两处排版差异；`rustfmt --edition 2024` 仅格式化本文件后复跑通过 | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action clippy` | 通过；仅既有 `proc-macro-error2` future-incompat 提示 | 0 |
| `pwsh -NoProfile -File ./build.ps1 -Action wasm-check` | 通过；同上提示 | 0 |
| `git diff --check`（仓库根） | 通过（README CRLF 为 warning 非差异） | 0 |

付费 live 保持 ignored，未执行、未读凭据。

## 4. 最终源码哈希与 Git 状态

| 文件 | 行数 | sha256 | 与 2.0 轮关系 |
| --- | --- | --- | --- |
| `NoManCode/rust-app/src/server.rs` | 1195 | `d89f035c0fdb8ca8b3e316e73e2d8f80a3e134d535e6a66e743d20f1f9959667` | 未变（本轮无需改生产代码） |
| `NoManCode/rust-app/tests/turn_http.rs` | 2717 | `9b8d53617674c4c06d29aa52690fbfae6b9c8746d5aa37a14a928d9c1f94f954` | 补强前置/隔离/等待 + rustfmt |

`git status --short`：两源码文件 `??`；本目录报告与证据 `??`。HEAD `8f0ccdc…` 未变；未做任何 Git 写操作。

## 5. 失败 / 未执行 / 历史结果区分

- 本轮失败：无。中途一次 n09 内部失败（team 分支计数沿用旧值）已在同轮内修正并复跑通过，非交付状态。
- 未执行：live（付费 ignored）。
- 历史结果：2.0 轮的 143 通过/1 忽略快照与 review 2.0 的 CHANGES_REQUIRED 保留；本轮独立复跑全量同数通过。
- 继承证据：`PredecessorChanged` 引擎侧仍由 B `t10_historical_resume_loses_to_a_newer_turn`（`tests/session_turns.rs:1187`）覆盖，本轮该测试在 workspace test 中通过；HTTP 层映射由 `turn_command_error_maps_every_stable_variant` 单测覆盖。

## 6. 边界声明

- 本轮未改生产代码；`server.rs` 哈希不变，实现仍以 review 2.0 确认的版本为准。
- N05/N06 的非 ASCII/编码输入已实证到达服务端并返回 400（构造/发送若被拒会 panic，而非静默跳过）；报告不再使用「客户端边界」表述。
- 本报告仅为员工提交物；C review、Git 提交、验收、合并、发布均由经理与用户决定。
