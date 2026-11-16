# fixture 修复与 W2 嵌套证据

E 只读确认 B 工作树三个接管文件无未提交变化；E 普通合入 B `cded3ef0` 保留已提交生产 fence 和历史，之后以 `8cc0b017` 仅提交授权的三个文件。plugin tests 持有真实 controller 并完成恢复；两个独立连接共享同一域并发写；schema12 迁移 fixture 删除 schema18 所有者表和 marker 后构造真实旧库，保留原数据/竞态断言并检查 schema18。provider admission 在线程启动前取得同域真实 operation，在线程内 `test_sync` 执行原事务；原 provider redaction failed/cancelled 断言未删，依赖 B `ae97f960` 与 A `881e262d` 的保守 unknown 结算，须经理真实复测。

`8cc0b017` 起初将 runtime deferred COMMIT 失败转换为 sticky unknown 子进程。B 与 D 审查指出 A `78de71c9` 现明确确认 rollback 后不封门；该 FK fixture 是确定回滚，应在原 controller 中重试。E 随后提交 `3cbf357a`，将 `runtime_management_tests.rs` 恢复为 B `cded3ef0` 的原字节，完整保留既有实际 COMMIT 故障和同 owner 重试断言。经理如已合入 `8cc0b017`，还需合入此后续修复。未知结果仍由生产逻辑处理，不在此测试中伪造。

静态检查经理指出的 plugin 局部变量：`b02_shell_cas_and_cross_plugin_global_key_race` 的 `path` 仍用于并发后重开数据库；`independent_stores_cas_one_winner` 的 `dir` 仍用于最终重开断言，均非闲置。直接 `rustfmt --edition 2024 --config skip_children=true` 格式化本轮三个文件，`git diff --check` 通过；未运行 Cargo、真实进程或真实 probe。编译及 full2 成败仍待经理验证。

经理提供 W2/W7 27/27、W3 pair 和 Host 矩阵通过；E 只读复查 W2 integration 原件 `.local/lifecycle/u18-w2-w7-1/`：`manifest.json` passed、30 步和 27 份 case.json，27 个 `*-case-*.log` 各有一项通过，20 份主 `cases/case-*/host.stage`、四份 `cases/case-11/w05/{before,suspended,resumed,pending}-host.stage`；另有不叫 host.stage 的 stage 文件，整个 `cases/` 均需原样留存。未出现 creation-handle unknown 标记。没有主 host.stage 的 7 例包括 W05 及六项 artifact/handle/limit 测试，不应按文件缺失推断失败或清理完成。

特别保留 W05 的 `14-case-11.log`、`cases/case-11/case.json`、整个 `cases/case-11/w05/` 及 `.local/lifecycle/window-closures/u18-w2-w7-1/`。日志记载四个 helper 的原 wait 和三个已创建 worker 的 wait；这些是该测试保存的证据，经理需结合各 case 的原 process/Job/I/O 证据与窗口闭合记录裁决，不能仅从测试进程退出推断全部后代收束。E 不发布、不移动或删除证据。
