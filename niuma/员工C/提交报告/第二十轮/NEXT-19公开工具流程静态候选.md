# NEXT-19 C 公开工具流程静态候选

2026-10-31，员工 C / 6.1 Terra。依据[正式任务](../../../项目经理/任务/2026-10-31NEXT-19-C公开工具流程与最终验收接续.md)，本人核收基线 `2f1bc071ca4494404c8d0370604240aa29a93b55` / NoManCode `15f9e726327197f5105293d9997ad800a2cd4f9a` / Rust `e64f9c159d311b5134cd94230e9a8f71932c6205`，C 树 `codex/c/tool-call-workflow` status/index/unmerged 空。仅新增 Rust `tests/tool_call_workflow.rs`、更新 `NoManCode/docs/cli-first-release-acceptance.md`；未修改旧 `first_user_workflow`、产品代码、schema 或依赖。

本人固定提交 `b903963b13fb0fa53f04a7585379061d3380a6d6` / NoManCode `14c89bc76165b3b38d277c5aa0ca29a97fab3492` / Rust `9d6b09f50d0925f0644f59fbc048548ec6f5dcab`，提交后同分支、status/index/unmerged 空。完整 diff 以 `git diff 2f1bc071ca4494404c8d0370604240aa29a93b55 b903963b13fb0fa53f04a7585379061d3380a6d6 -- NoManCode/rust-app/tests/tool_call_workflow.rs NoManCode/docs/cli-first-release-acceptance.md` 查看；两路径合计 941 增/0 删，测试复用既有公开工作流的本地 Provider/CLI/Host 辅助设计，不复用或更改生产测试后门。

## T13 静态断言映射

1. `tool_call_workflow.rs:304` 以独立 fixture、初始不存在的 DB、真实 CLI/Host 和受控本地 Provider 初始化，凭据走 stdin，通过公开 CLI 建立路由、Project 配置/选择、预算和显式网络授权。所有业务状态由产品入口产生；测试仅写项目 fixture 和配置输入，不注入业务 DB。
2. `:601` 起首 Turn 按 Provider 脚本真实 read、拒绝 write、拒绝 run_command；每个审批核 Task/Turn/Session 与 call ID，拒绝前后断言对应文件和命令副作用不存在。完成后检查 Provider 收到唯一匹配的 read 结果、两条各自唯一且文案为审批拒绝的结果。
3. 同一 Session 用实际前序 Turn ID `session send` 创建不同 Turn/Task；次轮逐次批准写文件和命令，命令批准显式 `--acknowledge-command-network`，检查 notice、写前/写后内容、命令成功和文件副作用恰好一次。Provider 请求检查五个 call 各一个 tool 结果。
4. `:763` 起 CLI 首批读取有限事件、以游标逐条续读到第二 Task 的 `status=completed`，检查序号严格推进、重复序号不存在、每个审批的 requested/resolved 和每个结果唯一且归属于正确 Task/Turn/Session；被拒绝调用无 tool_start，批准调用各有一个 tool_start。断言按当前事件契约静态编写，实际可达性待动态验证。
5. `:554` 及测试后段，真实 Host kill/wait，记录两次 PID、退出与 waited；首次停止后读取原工作 DB/WAL/SHM 文件字节，分别写只保留的 `stopped-raw` 和独立 `stopped-query`，记录存在性、长度与 SHA256；重启后仅按文件字节复核 raw hash，不以 SQLite 打开、恢复或查询 raw。允许缺失 WAL/SHM 并如实记载；不直接写原 DB。重启同目录并 bootstrap，比较 context、两次审批列表和已有事件，检查 Provider 仍为 7 次、命令副作用未增加。
6. 用第二 Turn ID 公开发送第三 Turn 并完成，检查第 8 次 Provider 请求中五个历史调用结果仍各一条，内容与副作用保持。每次 CLI 输出检查合成 Provider 凭据、敏感拒绝参数及当前/历史 Host token；两次 Host 合并日志在结束后同样检查。fixture 以唯一临时目录保留并打印路径，便于后续采集。

`NoManCode/docs/cli-first-release-acceptance.md:124` 只登记此候选的计划及未验证边界，不预写通过或主线整合。原 `first_user_workflow` 的 Diff、Checkpoint/恢复冲突和重启覆盖仍须在最终 workspace 全量实际执行，不能用本静态候选替代。

## 检查与后续

仅新测试文件执行 `rustfmt --edition 2024` 与 `--check`，暂存 `git diff --cached --check` 通过；文档检出含 CRLF，Git 提示暂存时归一 LF，diff 仅末尾新增六行。未运行 Cargo、Host、测试、安装或重跑；测试尚未编译/执行，不能声称 T13 通过或 CLI 实际行为满足断言。B/D 继续冻结、本人已释放。

下一步经理按完整 diff 和上表静态 review；获唯一最终窗口后，才对固定候选执行 workspace check、显式 CLI build、完整 workspace test（含新旧公开流程）、fmt、all-targets Clippy 与 UI wasm-check，首失败停止并按正式任务保全证据。历史 N19-F01/F02 与 N18-F01 不因新候选自动关闭；NEXT-19 尚未联合验收或整合，完整项目尚未完成。
