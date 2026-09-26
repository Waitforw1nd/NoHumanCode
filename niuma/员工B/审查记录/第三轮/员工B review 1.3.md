# B-R3-01 review 1.3 — CHANGES_REQUIRED（剩余契约证据）

2026-09-26，经理 GPT-6。固定输入 `74229736ffca4f712a7564b3084110c51c58ea44`、其后纯测试补证 `52364a9f2260ef9bc37a003284f7d05d1d7ebf77`；不将当前 engine.rs 在途新增视为已验证。B 当前实际模型 GPT-6-sol，原执行标识保持。

F10 在 7422973 关闭：prepare 后第二次 read_safe_file 错误先调用 mark_workspace_change_failed，Store 条件 UPDATE 必须改变恰一行；标记持久化失败传播。定向测试断言文件未变、change=failed、工具错误，recover 后仍 failed。前 FS 两个测试已用原子发布 task/record ID、停止前 durable 状态及字节核对、ChildGuard kill/wait、非成功退出和重启稳定 ID 加固。C 独立只读结论与经理固定差异一致；此处 recover 失败状态单测不冒称崩溃测试。

52364a9f 的 W02 真实第二读取失败，两种失败/成功顺序均不污染成功聚合；W11 在迁移前实际保存 task、approval、event 行并逐一核相等，重复 open；W05 本机 symlink 创建成功、外部哨兵不变及 alias/禁止拼写；W09 双 restore 等待 gate、单 operation、complete 后 resume 写零副作用已有证据。cfg(test) 一次性故障由单槽改成 task ID HashSet，避免并行单元测试互相覆盖。经理已复核差异，最终组合仍需实跑。

## 剩余一次性补证清单

以下来自原契约 W01/W04/W09/W10 和补充1、2明确条文，不新增产品范围。B 在原测试/engine cfg(test) 范围完成；已有其他测试时指出精确断言即可，不能用测试名或原审批通过数代替。

1. W09：partial、unknown 后 resume 发新 write_file，批准后零文件写、无新 change，operation 不变；可用 Store 操作建立终态来验证封存分支，但该夹具不计为真实恢复/崩溃证据。实际 partial/unknown HTTP 已由 C 单独证明。增加 start/resume 与恢复共用 gate 的屏障，证明请求已进入而未持久创建/执行，并在释放后收集结果。
2. W01/W04：新建普通文件经真实批准写入，恢复删除；外部编辑新建文件后 restore 冲突且不能删除。
3. W10：超大新内容、超大/non-UTF8 before 在副作用前拒绝，原文件字节不变且无成功 change。秘密新参数拒绝和 DPAPI 秘密 before roundtrip 已有，不重复替代这些边界。
4. 补充2 §7：当前 Host workspace 或任务 scopes 改变，restore 拒绝、零文件写/无 operation；补充1 的 A 写→B 改→A 再次 write 防止吞并中间变更。

C 最终支持 `cf1a9c2c581985d1ce8a8907f04898df7c46c99f` 已按其 review 1.1 限定通过，覆盖真 HTTP 和执行中第二文件故障 partial。B 完成上述后明确干净冻结，经理串行合入 C 测试，通知 B 跑 check/test/fmt/clippy/wasm-check 五标准门禁；不发布 build/付费 live。

正式报告必须逐项 W01～W14，列真实公开 API（当前 Engine 返回 anyhow::Result，业务 WorkspaceChangeError 可 downcast，不能伪写强类型 Result 别名）、原/新模型贡献、完整候选 SHA、命令起止/退出码/日志及支持边界。尚无 B 第三轮完整实现报告；提交和支持 review 不构成 Workspace 主线验收。

2026-09-26T21:32:56+08:00 临时503续接时 B engine.rs 在途61行原字节保全于 `.local/review-evidence/B-R3-503-20260926-213256/engine.rs`，原件/副本 SHA-256 均 `9D4953996C0F6615B808DFCF02058B39F3D5E2CC6D4B418964700027675621D9`；原代理继续，无覆盖/清理。
