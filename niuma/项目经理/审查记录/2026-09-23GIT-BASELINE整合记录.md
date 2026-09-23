# Git 整合记录：GIT-BASELINE 首次完整基线

日期：2026-09-23；整合人：项目经理；状态：已合并（main 快进）。

- 任务、报告、review：任务单 [2026-09-23GIT-BASELINE收尾](../任务/2026-09-23GIT-BASELINE收尾.md)；盘点证据 [2026-09-23NEXT-01工作区基线.json](2026-09-23NEXT-01工作区基线.json)、[2026-09-23源码迁移清单.json](2026-09-23源码迁移清单.json)、[2026-09-23目录迁移清单.json](2026-09-23目录迁移清单.json)；验证证据 [2026-09-23C-D联合门禁](2026-09-23C-D联合门禁.md)；员工验收 [员工C review 2.1](../../员工C/审查记录/第二轮/员工C%20review%202.1.md) 及 A/B/C 历史 review。
- Git 模式、分支、工作树：共享工作树 `./`；候选分支 `codex/manager/baseline` 就地创建于含全部有效成果的工作树（不从旧 HEAD 另建）；经理是唯一暂存/提交/整合执行人。
- 来源基线完整 SHA：`8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5`（main 旧 HEAD，另有已清点的未提交迁移与实现内容，见任务单候选分组）。
- 候选实现完整 SHA：`b67fedfcd6ca35969363096ea64ddd5fb1290524`（`chore(repo): capture reviewed backend and workspace baseline`）。
- 报告/review 所在后续提交：本记录及同步文档所在 docs 提交，SHA 下轮交接登记。
- 整合方式与目标：`git fetch . codex/manager/baseline:main` 本地快进至 `main`，无 merge commit；`codex/manager/baseline` 分支保留作基线标记，清理按 §6 另行核对。
- 整合后 SHA：`b67fedfcd6ca35969363096ea64ddd5fb1290524`（main 当前指向；不含本文件所在的后续 docs 提交）。
- 内容差异：383 文件、+65506/−3891；name-status 构成 288 A + 79 R + 10 D + 6 M。89 个旧路径删除全部配对现存目标：81 项源码迁 `NoManCode/`、3 项 `records/` 迁 `niuma/项目经理/项目记录/`、5 项员工报告迁 `niuma/员工A/提交报告/`。10 个删除因内容相似度低于改名阈值在 staged diff 中显示为 D+A，目标均已核对在提交内：`dsh-peachsh.cmd`、`newapi.cmd`、`start-peachsh-legacy.cmd`→`NoManCode/`；`rust-app/build.ps1`、`rust-app/src/{repository,secrets,server}.rs`→`NoManCode/rust-app/` 对应路径；`records/{README,PROJECT-PROGRESS-SUMMARY,CONVERSATION-FULL}.md`→`niuma/项目经理/项目记录/`。CRLF→LF 入库规范化：`README.md`、`package.json`、`NoManCode/overrides/client.js`、`GOAL-ROADMAP-DRAFT.md`、`tests/{assessment_adversarial,live,runtime}.rs`、`web/{app.js,style.css}`、`tests/dump-config.txt`（测试/短路径均相对 `NoManCode/rust-app`）；4 个 `.cmd` 按 eol 属性保持 LF blob、检出 CRLF。`assessment/2026-09-20-live/` 下 `baseline.log`、`adversarial.log` 被 `*.log` 忽略，按任务单证据例外 `git add -f` 纳入（该区域 `-text` 字节保留）。无 >500KB 文件、无凭据、无 `target/`、`node_modules/`、`data/`、`.local/` 入库。
- 实际验证：被测版本即本候选工作区内容。提交前：逐文件清单暂存（非 `add -A`）、staged name-status 复核、旧新路径配对核对、`git diff --cached --check`（仅历史文档/日志行尾空白提示，字节保留证据不修）。功能验证：[2026-09-23C-D联合门禁](2026-09-23C-D联合门禁.md) 经理实跑 `build.ps1 -Action test` 143 通过 / 0 失败 / 1 付费忽略，fmt、clippy、wasm-check 退出 0；历史 99 项通过为引用证据，未作全量重测。rust-app 86 文件与 [派发前基线 JSON](2026-09-23NEXT-01工作区基线.json) 逐文件比对仅 `server.rs`、`lib.rs` 两处任务内预期差异。
- 来源 SHA 与新 SHA 的映射：不适用（单快照承接积压迁移，无重写/移植；不伪造员工各自提交，历史贡献在提交正文标明）。
- 尚未完成与下一步：未配置远程，未推送、未发布。首次完整基线已登记，后续员工任务可按任务单从 `b67fedf` 派生独立工作树；`codex/manager/baseline` 标记分支待核对后清理。
- 回退：不适用。若需撤销按 Git协作规范 §6 新增 revert，不 reset 主线。
