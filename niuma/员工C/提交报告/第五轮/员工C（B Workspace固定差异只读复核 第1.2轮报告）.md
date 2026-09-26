# 员工C（B Workspace固定差异只读复核 第1.2轮报告）

2026-09-26，员工 C，当前实际模型 GPT-6-astra。用户明确“改用 6 astra”后由经理接续派发；本轮首次本机读取登记时间为 `2026-09-26T22:29:19.4027556+08:00`，不将该时刻伪称消息送达的精确秒数。沿用任务 `C-R5-CRASH-SUPPORT` 与执行标识 `C-R5-CRASH-SUPPORT-20260926-034330`。旧 GPT-6-sol 的测试、提交、失败与验收事实保持。

## 输入与边界

- 只读差异：B `74229736ffca4f712a7564b3084110c51c58ea44` → `52364a9f2260ef9bc37a003284f7d05d1d7ebf77`，仅 `NoManCode/rust-app/src/engine.rs` 与 `NoManCode/rust-app/tests/workspace_changes.rs`，359 行增加、15 行删除。engine 的变化均在 `#[cfg(test)]` 中，本差异不改变生产逻辑。
- B 固定 blob：engine `8cabf883b6e8d241b6bfa69958defd0d0a9d643b`；workspace_changes 测试 `3027e46b6b7cf5c4a72e4180b531cb7fa1dbbe0d`。B 工作树存在 engine 在途修改，未纳入本报告。
- C 源码工作树 `../nhc-c-workspace-crash/` / `codex/c/workspace-crash-tests` 的 HEAD 仍为 `cf1a9c2c581985d1ce8a8907f04898df7c46c99f`，读取时干净。C 本轮未改任何产品源码或测试、未操作共享 index、未运行产品测试，不自审既有 C 测试。
- 已读 B 原契约及补充1～3、[B review 1.3](<../../../员工B/审查记录/第三轮/员工B review 1.3.md>)、C 第五轮任务、[review 1.0](<../../审查记录/第五轮/员工C review 1.0.md>) / [review 1.1](<../../审查记录/第五轮/员工C review 1.1.md>) 和上一份只读报告。本报告是独立静态复核，不将历史执行结果称为 C 实跑。

## 固定候选结论

| 项目 | 固定代码能证明的内容 | 本轮结论与限制 |
| --- | --- | --- |
| W02 失败聚合 | engine:2042 `failed_prepare_does_not_pollute_later_or_earlier_success` 走本机 mock、真实批准 write_file。按先失败后成功、先成功后失败分别触发 prepare 后读取故障，核两条 durable 记录的 failed/finished、最终 after 字节、公开 changes 只聚合成功一项且可恢复，restore 后为首次 `before-a`。一次性故障槽改为 task ID HashSet，避免不同测试覆盖。 | 断言与故障位置符合失败不污染成功聚合要求。不是用 SQL 伪造成功；未独立执行。 |
| W05 链接与边界 | workspace_changes:853 在真实成功写入后将目标换为 symlink，成功创建时核 Invalid/UnsafeLink、恢复拒绝及外部哨兵不变；不能创建时输出实际系统错误。:897 核 Windows 大小写 canonicalize 相同、尾点/尾空格/穿越/受禁名拒绝。既有 :699 覆盖真实 hardlink 其他目录项字节保留。 | 这些代码断言覆盖所述场景；本轮未运行，不能单凭条件分支宣称当前机器 symlink 已执行成功。成功/不可创建需以 B 最终原始日志为准。别名 resolve 断言本身不等于真实两种别名写入聚合测试。 |
| W11 迁移保全 | workspace_changes:952 先经真实批准写入建立业务事实，采样 task.value、approval 的 id/status、event 的 seq/kind/data，删除 schema8 专属表/标记后从 user_version=7 open，核 schema8 与所采样数据相等并再次 open。既有 approval_gate:936 的 AP19 删除 schema7/8 专属对象后设 version6，迁移故障回滚保持6，再正常 open 到8并保留 task.value。 | 7→8 的真实非空数据夹具与6→7→8连续路径均存在，上一报告“仅空库迁移”的差距已部分补足。但这里只比较所列列值，不能表述为所有 approval/event 列逐字节全量保全。伪 CHECK 两字面量与索引列顺序反例已见；类型/NOT NULL/FK/唯一属性/partial谓词由现有 DDL/pragma 校验静态覆盖，未在该固定差异见各自独立的坏结构实跑证据。 |
| W09 并发与封存 | engine:2073 手持 Engine gate 后 spawn 两个 restore，释放后核两 receipt 完全一致、complete、文件 before 与 operation 数恰为1；随后真实 resume/mock 新 write_file/批准，核文件仍 before、receipt 与 change 数不变。生产 start/resume/restore/write_file 入口静态可见共用 gate。 | complete 后写封存断言明确。并发屏障仍有下节 C-W09-E1 证据问题；partial/unknown 后写封存及 start/resume 与 restore 竞争仍归 B review1.3 补证。 |

## C-W09-E1：到达信号早于实际轮询 gate（P2，测试证据）

位置为固定 engine.rs:2085 起的两个 spawn：先 `entered_a.send(())` / `entered_b.send(())`，然后才调用 `engine.restore(...).await`。父任务收到 ready、`yield_now()` 后只核 `JoinHandle::is_finished`，不能严格证明两个 restore future 已被轮询至 gate 的 Pending；信号证明的是子任务运行到调用前。若调度在 send 后暂停子任务，即使拿掉 restore 内的 gate，中间“未完成/无 operation”的断言仍可能通过，之后调度顺序执行又可得到同 receipt。该问题不等于生产并发错误，但不足以作为契约要求的确定性前置屏障。

建议在当前 cfg(test) 范围直接 pin restore future，用 `std::future::poll_fn` / `Future::poll` 显式轮询一次并断言 Pending，再向父任务发送到达信号，随后继续 await 同一 future；或者以等效的、实际位于入口 gate 等待处的测试探针证明到达。start/resume 新竞争测试应采用相同原则。释放 gate 后使用已有有界 timeout 风格收集各任务，避免 gate 回归时测试无限等待。C 不修改 B 文件；已先将此具体建议发给经理。

## 未完成与下一步

本轮未发现该固定测试差异新增生产缺陷；不能由此将完整 Workspace 标为通过。B review1.3 的四组范围保持：partial/unknown封存与 start/resume 屏障、新建文件恢复/外部改动、超大或不支持字节拒绝、Host/任务scope变更与 A写→B改→A再写。W11 原契约的证据完整性应在 B 正式 W01～W14 报告中逐项列清，不能以本报告静态确认替代未有运行记录的场景。

只读执行过 `git status --short`、完整 HEAD/分支、固定 `git show` / `git diff` / blob 核对；`git diff --check 7422973 52364a9f` 退出0。没有执行 cargo、build、付费调用或发布，故本轮产品测试数为0。B 第三轮正式实现报告在本次读取时尚未出现；最终组合五门禁仍待 B/经理。

下一步第一条操作：取得 B review1.3 补证后的完整固定 SHA与冻结声明，只读核其相对 `52364a9f` 的差异及原始证据，新增后续报告；已交本文件和旧报告保留历史，不回写更改原结论。最终验收由经理决定。
