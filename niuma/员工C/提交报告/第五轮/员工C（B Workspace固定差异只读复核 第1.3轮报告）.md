# 员工C（B Workspace固定差异只读复核 第1.3轮报告）

2026-09-26，员工 C / GPT-6-astra；沿用 `C-R5-CRASH-SUPPORT-20260926-034330`。经理提供 B 固定 `2578029682e18eea911d5fae99717fb8beafe2ae` 与明确干净冻结声明后接续；本轮首次本机读取登记 `2026-09-26T22:35:58.4432835+08:00`。本报告新增归档，不覆盖[第1.2只读报告](<员工C（B Workspace固定差异只读复核 第1.2轮报告）.md>)。

## 结论与受审输入

**C-W09-E1 已关闭；B review1.3 四组所列缺口已有对应固定测试与定向执行证据，可交经理关闭。** 本差异未发现新增生产阻断。结论限于此次补证，不宣告全 W01～W14、最终组合五门禁或完整 Workspace 已验收。

输入为 `52364a9f2260ef9bc37a003284f7d05d1d7ebf77` → `2578029682e18eea911d5fae99717fb8beafe2ae`，只有 `NoManCode/rust-app/src/engine.rs` 的 `#[cfg(test)]` 部分及 `NoManCode/rust-app/tests/workspace_changes.rs`，444行增加、25行删除，无生产逻辑变化。固定 blob 分别为 `d69c151e03a31d47629746c99ac8d123708a7067` 与 `b011dd5c50b1ab0bb7d7a6eaff9e01661dfa7663`。第一次读取 B HEAD 为受审完整 SHA，`git status --short` 无输出；之后经理串行整合不改变本报告固定输入。

C 只读源码/日志并写本人报告和身份；未改产品源码、测试或共享暂存区，未运行 cargo/五门禁、未自审 C 已限定通过的支持测试。以下运行结果均归 B，不冒称 C 独立实跑。

## 逐项关闭依据

| 项目 | 固定源码与断言 | 结论和覆盖边界 |
| --- | --- | --- |
| C-W09-E1：真实 gate 等待 | engine:1661 `assert_gate_pending` 用 `std::future::poll_fn` 对 pin 后的同一 future 调用 poll，直接要求 Pending。:2081 双 restore 都在持有 Engine gate 时完成该步骤，未释放前 operation 不存在、字节仍 after；释放后8秒 timeout 内 join 两个原 future，同 receipt/complete/单 operation/精确 before 断言保持。 | 原“先发进入信号、再调用 restore”的调度漏洞已消除。此处 restore 首个 await 就是 gate，不能由后续非 gate 等待冒充。 |
| W09：start/resume 与恢复协调 | engine:2257 持有同一 gate，依次把 restore/start/resume 三个生产 future 轮询到 Pending。释放前 Store 查询的 runs 列表及原 task JSON 完全不变、Engine非busy、无restore operation、文件after不变；释放后8秒内收集三结果，核restore complete、创建1个task的新run、resume原task及runs数量增加1、文件为before。随后cancel_all并8秒内等待无busy。 | 此用例证明三入口实际等同一gate，并验证restore先排队后的释放路径。数据检查是runs列表/原task/operation与文件，不夸大为每张表的全量快照；没有声称覆盖所有调度排列或跨Host互斥。 |
| W09：partial/unknown 后写封存 | engine:2168 通过 Store claim_restore/finish_restore 建立两种终态，核逐路径Unknown及不可恢复；真实resume→mock返回phase-3 write_file→用户批准→任务结束，文件仍after-1，change ID集合不变，receipt相等，工具error，approval execution=Finished，SQL count(workspace_restores)=1。 | 封存分支证据齐备。构造的partial没有真实已完成路径，明确仅测已有operation封存；不是本轮真实partial/崩溃证据，后者保留C已有限定review来源。 |
| W01/W04：新建文件恢复与外部改动 | workspace_changes:195 真批准write_file创建new.txt，核Created、before_digest=None、Finished、restorable及精确CRLF字节。未被改动时恢复complete/restored=1/逐路径complete并删除，重复Engine restore返回同receipt；被外部编辑时typed Conflict、保留外部字节、无operation、current=Diverged。 | 两个新建分支均有实际文件状态和typed拒绝断言，可关闭。本项直接调用Engine，不冒称真实HTTP POST。 |
| W10：超大/非UTF8无文件副作用 | workspace_changes:241 覆盖262145字节新内容、262145字节before、非UTF8 before；原文件逐字节不变、安全工具error、审批approved/finished、无restore及目录仅原文件。超大新内容直接SQL核唯一state=failed；另两项公开changes及SQL states都空。 | 既核公开聚合又核原始行，避免“聚合隐藏prepared/finished”误判。超大新内容允许有failed准备行，未将“没有成功change”夸为“从未prepare”；原契约要求文件副作用前拒绝得到验证。 |
| 补充2 §7：Host workspace和task scope变化 | workspace_changes:305 Host分支通过真实configure改变workspace并读回；scope分支先断言普通save_task拒绝不可变scope，再仅对临时DB tasks.value作SQL UPDATE、要求影响1行，并Store读回["src","extra"]。restore必须typed Conflict，原工作区after和另一工作区哨兵均不变、无operation，change身份/Finished/pending保持。 | Host配置路径与持久化篡改防御分别得到验证。scope不可通过公共save_task合法更改，SQL只为绕过该前置保护触达恢复快照检查，不表示产品公开允许改scope。 |
| 补充1：A写→B写→A再次写 | workspace_changes:370 同Engine/脚本mock依次运行A、B，再resume A返回a-second并批准。字节仍task b，A的change身份、before/after摘要保持；工具error、approval Finished、无A operation。A恢复typed Conflict；先恢复B得到task a，再恢复A得到original。 | 真实两任务顺序及A再写拒绝清楚，恢复链反向结果排除吞并中间编辑，可关闭。 |

固定生产入口再次核对：start:201、resume:1041、restore:1269 首先等待 `self.gate.lock().await`；restore权限对比发生在claim之前。此处仅用于验证测试停点/拒绝原因，并未扩展审查范围或修改实现。

## 实际日志、首次失败和哈希

原始日志实际位于共享仓库根 `.local/verification/B-R3-astra-20260926-224100/`，不在 B 源码工作树。发布证据在[员工B第三轮证据](../../../员工B/提交报告/第三轮/B-R3-astra证据/manifest.json)。已读取[命令时间与退出码](../../../员工B/提交报告/第三轮/B-R3-astra证据/commands.jsonl)及相关输出，并逐项用SHA-256核对manifest的8份原始文件和8份发布文件，全部匹配。

| B命令 | 实际起止（+08:00） | 已核结果 |
| --- | --- | --- |
| build.ps1 -Action check | 22:32:52.778 → 22:32:55.667 | exit0 |
| cargo test --locked --lib workspace_phase_tests | 22:32:55.686 → 22:33:06.501 | 7 passed、0 failed；exit0 |
| cargo test --locked --test workspace_changes（首次） | 22:33:06.507 → 22:33:16.475 | 20 passed、1 failed；exit101 |
| cargo test --locked --test workspace_changes（修正后） | 22:34:01.133 → 22:34:09.047 | 21 passed、0 failed；exit0 |
| build.ps1 -Action fmt | 22:34:09.063 → 22:34:10.101 | exit0，stdout日志为空 |
| build.ps1 -Action clippy | 22:34:10.103 → 22:34:15.048 | exit0 |

首次失败精确位置为scope用例原先对 `save_task` 的unwrap，错误“任务成员、模型、路径与创建时间不可变”。这证明普通保存提前拒绝，尚未触发欲测restore分支；最终固定测试按上表先保留该拒绝断言，再显式SQL篡改临时夹具，实跑21/21。没有更改生产限制或删掉安全目标。失败原日志与exit101完整保留。check/Clippy还输出依赖proc-macro-error2的future-incompatibility警告，命令实际exit0，未宣称无警告。

这些命令早于22:35:01固定提交；B提交说明登记它们为本候选定向结果。首次失败属于更早测试夹具，不能称为固定最终候选失败，也不能抹为从未失败。最终组合五门禁由B继续运行，作为完整受测版本锚点。C没有另跑这些命令，产品测试执行数仍为0。

## 剩余与交接

本轮四组补证与C-W09-E1可以关闭；没有另加产品返工项。W11各字段/坏结构的覆盖边界继续按第1.2报告和B正式矩阵准确说明，不在此次把静态覆盖写成新实跑。F3沿用已确认RAII加pre-replace实际故障证据，未逐点注入write/sync作为覆盖限制，不当作新增生产失败。

尚待经理合入C已验收支持后的B组合五门禁、正式第三轮W01～W14报告、经理最终review与主线整合。只读 `git diff --check 52364a9f 2578029` 退出0。下一步第一操作为收到组合完整SHA、五门禁和正式报告后，对本次固定补证的版本对应与说明作必要核对；不重跑/重开C已限定验收测试，不先行宣布完整Workspace通过。
