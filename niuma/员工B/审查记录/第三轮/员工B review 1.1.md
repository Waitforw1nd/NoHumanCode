# B-R3-01 review 1.1 — CHANGES_REQUIRED（修复中间候选）

2026-09-26，经理 GPT-6，固定 `083eae51820a008d02197c58b0dcfdf2ff7ba775`，相对 `dd57adfc9321454651bdcdabb3a081bc95091600`。六处修复差异在授权范围内。B 的 check 0、workspace_changes 9/9 与 C 在该实现上的后窗口 6/6 为中间证据，不是 W01～W14 全部完成或五门禁通过。C 的 Clippy 暴露五处 lint，已交 B；新 A 代理启动因系统 thread 上限未成功，本次是经理固定差异审查，不冒充独立 A 复核。

| 项目 | 本 SHA 静态结论与剩余证据 |
| --- | --- |
| F1 | 两新路由前置共享 ID 校验，固定 4xx/500 安全文案；真实未知内部错误/授权/HTTP 矩阵仍待完整证据。 |
| F2 | outcome/parent 持久化失败 best-effort unknown，DB 仍故障也返回稳定 restore_id；已有两窗口测试，重启与完整逐路径证据由组合补齐。 |
| F3 | RAII cleanup 已覆盖 write/sync/pre-replace/replace 返回错误；当前目录目标测试命中 pre-replace 校验失败。其他注入窗口仍需适用证据。 |
| F4 | failed 已在聚合与恢复候选中排除；unknown/prepared 仍阻断。failed→finished 及反向真实测试待补。 |
| F5 | task 有 legacy file_backup 时恢复 fail closed，mixed fixture 已有；完整旧版本迁移/恢复矩阵待补。 |
| F6 | finish_restore 终态事务同步剩余 outcomes/change，任意 operation 封存 restorable；但 recover 路径仍缺 change.restore_state 同步，见下。 |
| F7 | 恢复从 durable 原 tool call 重算 path/content/binding，新增损坏反例；带脱敏损失的新写入在当前版本会成功后不可恢复，见下。 |

## 必须补齐的修复

**F6 延伸：恢复进程崩溃后的 change 状态。** Store::recover 当前只将 restore parent/outcomes 的 claimed 改为 unknown，却没有同步对应 workspace_changes.restore_state。重启后的 receipt 为 unknown，GET changes 仍为 pending。应在同 recover 事务按未完成 outcomes 更新 change 为 unknown；已 complete/restored 路径保持。C 真实 HTTP/子进程崩溃测试增加 DTO 一致性断言。

**F7 延伸：脱敏参数导致成功写入不可精确重核。** B 通过真实 mock/审批流程复现 `sk-workspace-test-secret` 内容获批写入并 finished，但 durable args 被脱敏，之后恢复 binding 校验失败。经理裁决：本切片在 prepare 和文件副作用之前拒绝会丢失精确持久表示的新 write_file args，返回安全工具错误，不能执行替代值；不新增原 args 密文或修改 schema 保存秘密参数。W10 要求的是含秘密的原 before 经 DPAPI 精确恢复，可以用普通 after 覆盖后再恢复；不要求放宽 AP 的脱敏参数约束。反例必须证明无文件写、无成功 change，原 AP14 继续回归；报告明确支持边界。

**F8：schema8 SQL 规范化改变了字符串字面量语义。** verify_schema_8 的 split_whitespace/to_lowercase 应用整个 SQL，将 CHECK 中 `'CREATED'` 与 `'created'`、`'pre pared'` 与 `'prepared'` 视作等价。实际 SQLite CHECK 行为不同，伪装结构可被接受。需保留引号内字面量的词法规范化或约束行为校验，以真实伪装 CHECK 反例证明拒绝，正常迁移仍保全。原文件范围可修复，不需要新写权。

**F9：损坏 finished 记录被静默过滤成恢复成功。** 当前 schema 允许 finished 行 after_digest 为 NULL；restore 以 finished 且 after_digest 存在过滤，单条损坏记录会被丢弃并返回 complete/0，实际文件没有恢复。应验证 finished 必有合法 after 摘要并 typed Corrupt 零写，未知枚举不得默认当 failed 后忽略。以合法 SQL 更新 after_digest=NULL 的真实反例验证；确定 failed 仍按 F4 排除。

副作用前真中断测试可由 B 在原 engine.rs 内添加仅 cfg(test) 私有单元测试 barrier；禁止生产 hook/公开控制接口/Cargo feature/正式运行时环境后门。C 独立文件负责副作用后及提交后真实子进程和 HTTP 窗口。最终先固定组合，再跑五标准门禁与全部 W/AP/AH 回归。

本次不验收、不合 main。已验收审批 CLI 不在返工范围。员工继续原任务与执行 ID，真实模型换班记录不覆盖历史。
