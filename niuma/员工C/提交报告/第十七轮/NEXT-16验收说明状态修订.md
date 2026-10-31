# NEXT-16 CLI 验收说明状态修订

2026-10-31，员工 C / 6.1 Terra，C-R17-01-20261031-E01。经理明确只解冻 `NoManCode/docs/cli-first-release-acceptance.md`：修正过期 NEXT-15 最终门禁状态，明确其已联合验收并整合主线；NEXT-16 空数据故事仅 C 组件已通过，B/D Provider 新内核组合与最终门禁仍待验收。九月 57f750b 统计与当时未放行事实明确标为历史，不改数字。

补充现行主线 `plugin list/status/set-enabled` 管理 builtin.files 与 builtin.shell，不管理 Provider；NEXT-16 Provider 候选的 Rust 进程内生命周期与状态、关闭新准入和重启默认重新启用，不冒称主线 CLI 或持久管理功能。默认插件完整组合仍待逐项验收。

本人独立树 commit `46cf08d584e2136cc8493b848e6a7cec88a3be0f`、NoManCode tree `0b2c4adc9b888a344c502ec2498b7b01028f1dcc`、Rust tree `183eb11234887c6f0d9946f140b7e931ef24c264`；仅文档变更，Rust tree 与受测 `74f40f3e` 完全相同。`git diff --check` 和 `git diff --cached --check` exit0；status/index/unmerged 空。未构建、测试或修改测试/server、schema、权限或凭据。本人持续 source/index clean/no-build，D 保持唯一重型窗口。下一第一步等待经理新组合及精确放行，本人核三 SHA 后按规定测试。
