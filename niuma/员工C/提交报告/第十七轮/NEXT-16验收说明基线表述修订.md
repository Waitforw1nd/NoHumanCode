# NEXT-16 CLI 说明基线表述修订

2026-10-31，员工 C / 6.1 Terra，C-R17-01-20261031-E01。经理审查后要求进一步避免合入后“当前主线”描述失效，仅修改 `NoManCode/docs/cli-first-release-acceptance.md`：明确 NEXT-15 基线的 Provider Completion 路径，以及 NEXT-15 基线和本候选 CLI 的 files/shell 管理范围；NEXT-16 Provider 候选仍未完成组合验收，不以其为全部默认插件已完成。

本人树文档提交 `b596b67a316d2d0aaacd1e632e9e7498d7777bc6`，NoManCode tree `ce7da47e9698db76352917d260c482d52e5bafb8`，Rust tree `183eb11234887c6f0d9946f140b7e931ef24c264` 与受测 74f40f3e 保持相同。仅两行文档变化，`git diff --check` 与 `git diff --cached --check` exit0，status/index/unmerged 空；未构建/测试，保持 source/index clean/no-build。下一第一步等待经理 D 候选整合及精确验证放行。
