# NEXT-13 第0.6轮N13-D09格式返工

员工D GPT-6-astra，2026-09-28，同D-R11-01-20260927-E01。独立树 ../nhc-d-git-worktree / codex/d/git-worktree；本人提交git_worktree.rs，经理整合。开始527f466 clean/index空/no-build；实际核C f45f2a8与本人527的git_worktree blob均c6a5520a03176576da092e4df92b36c5d117e97b。

根因已实核：Cargo.toml edition=2024，但此前两文件检查显式--edition2021，检查的style edition与workspace实际不同；此外历史从repo根调用rustfmt命中1.7.1-stable，而Rust cwd的rust-toolchain覆盖命中1.9.0-stable。版本/根到Rust目录及祖先rustfmt配置不存在的探测记录于d09-context.json。此前没有skip_children参数。不能用旧two-file fmt0代称workspace fmt通过。

只读cargo fmt --all -- --check在本人527同样exit1，四处imports/eprintln/签名/assert差异与C一致，before原始log/meta保存。随后仅在Rust cwd用rustfmt --edition 2024 src/git_worktree.rs格式化本人文件，没有全仓改写；diff检查只有该文件，纯格式/import排序。提交后cargo fmt --all -- --check实际0。

固定 **fac79babc45decc9adc778c732f2e7839ac1e470** / NoManCode **9d3269526f78c3a7d89aa152c0da74fc3d87eb9a** / Rust **7b9b238ee6050a7b1a71a8c8b2fd91fa4908b6f8**；status/index空、无构建，源码冻结供经理整合。未重跑已过功能组件、未跑最终五门禁。5对新增原始/发布hash见[evidence-0.6清单](evidence-0.6-manifest.json)，原失败保留。

下一第一操作：等经理整合与C新最终门禁；后续格式命令在Rust cwd使用workspace实际edition/toolchain，整体格式验证使用cargo fmt --all -- --check，不能硬编码旧edition作为验收替代。