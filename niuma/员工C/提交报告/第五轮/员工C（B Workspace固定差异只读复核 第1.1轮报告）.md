# 员工C（B Workspace固定差异只读复核 第1.1轮报告）

2026-09-26，实际模型 GPT-6-sol；经理在 `2026-09-26T20:40+08:00` 后接续派发。只读核对 B 固定 `268c0e49203933077480f1680a9dc372486ceb44` 的 F8 夹具和固定 `50d406b0c796eff4c3e77dec7755eda1cb196aa2` 的前 FS 测试；后者所在 B 工作树有在途修改，本报告只以固定 commit 为判断输入。未修改 B 源码、C 测试或共享暂存区；未运行产品测试，不能冒充 B 的正式执行结果。此前 [第1.0轮差异复核](<员工C（B Workspace固定差异只读复核 第1.0轮报告）.md>) 的 B-F10 保持待关闭。

## 固定差异结论

- **F8 夹具的原误因已关闭（代码证据）**：`268c0e4` 保存三张 schema 8 表及三个索引的原 DDL，删除后重建全部结构，只有 `workspace_changes` 的 CHECK 字面量被替换为大写或内嵌空格变体；`Store::open` 必须返回包含 `workspace_changes` 与“约束”的 schema 校验错误。这样缺表、缺索引不再能解释测试通过。两个 forgery 输入长度不同，测试临时库路径也不同。未由 C 独立运行本测试，结论限于夹具审阅。
- **`50d406b` 前 FS 夹具仍不足作固定验收证据**：测试专用 `#[cfg(test)]` probe 置于 write durable prepare/`tool_start` 后、`workspace::execute` 前，以及 restore durable claim 后、逐文件 action 前；停点位置符合目标。但 child 以普通 `std::fs::write(phase.signal, task_id)` 发布信号，parent 只等 `exists`，可能在 create 后写完前读到空/不完整 ID。parent kill/wait 后才查看 change/restore；kill 前没有持久 prepared/claimed 行、审批 claim、restore_id 和目标文件字节的完整前置断言。子进程没有 RAII 清理，超时/断言 panic 可遗留。restore 只查最新 receipt status，未验证稳定 restore_id 及逐路径 change 归属。经理已要求 B 修原子发布、RAII 和前置核对；B 在途改动不能计为 `50d406b` 固定证据。

## W09/W11/W13 缺口

| 契约项 | 当前已见 | 仍需可固定证据 |
| --- | --- | --- |
| W09 | 单任务顺序重复 restore 的同 receipt；A/B 跨任务写后旧任务恢复冲突 | 并发 restore 同 operation 的确定性屏障；restore 与 start/resume/新 write 的先后竞争及封存 task 写入；不能以顺序测试代替并发 |
| W11 | 空库模拟 schema7→8、重开、一个伪 index、F8 的两类 CHECK 字面量反例 | 真实旧业务/审批/事件行保全；schema6→7→8 连续迁移；伪列类型/NOT NULL/FK/唯一与部分索引等结构独立反例；每个故障确认拒绝原因 |
| W13 | 真实 HTTP outcome 持久化失败返回安全 unknown；C 的 crash 测试覆盖重启后 GET receipt、重复 POST unknown/complete | 完整 DTO 与正常/partial 路由；Host/Origin/token/安全头拒绝；非法 persisted ID、未知 task 与空 task 区分；500 不泄 SQL/路径/秘密的独立故障输入 |

上述是固定 SHA 和已有测试源码的证据盘点，不是新增生产缺陷断言。B 当前继续实现/测试中；最新门禁与 W 矩阵要等其固定新 SHA、报告和原始日志，C 不引用旧轮全量测试数。下一步只读审 `B-F10` 修复差异、更新的前 FS 夹具与缺口证据，交经理决定是否关闭。
