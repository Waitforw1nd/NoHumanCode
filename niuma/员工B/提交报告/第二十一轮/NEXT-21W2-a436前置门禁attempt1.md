# a436 前置门禁 attempt1

2026-11-02 员工B。经理 [复审并签窗](../../审查记录/第二十一轮/2026-11-02a436前置外围复审与签窗.md)，启动前核 a436/双tree、clean/index、四外围hash、48份旧原件均一致，未见匹配B进程。UTC 2026-11-01 23:26:51 独占打开旧锁成功，绝对路径核界、目的不存在，保全迁移如下：

- 原：`../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/nhc-w2-window.lock`
- 新：`../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-gates-b-f898-attempt3/nhc-w2-window.original.lock`
- 前后 SHA256 `1913b9fa7a99ed3b0c7c5d60111308509f329d15014fd92a78a453b1b40757f4`；源位置不存在，原清单未变。

下一步执行已签的独立 check→Clippy 前置，持续持有工具会话。

## 实际结果及发布

原工具会话16812退出0；supervisor 原句柄 PID 82020、creation FILETIME 134380492309280011，UTC 2026-11-01 23:27:54.9159063 signal/exit0。两步串行：

| 门禁 | 开始 UTC | 结束 UTC | 原PID / creation FILETIME | exit |
| --- | --- | --- | --- | --- |
| workspace all-targets check | 23:27:12.7482269 | 23:27:31.2892870 | 78904 / 134380492327565295 | 0 |
| workspace all-targets Clippy `-D warnings` | 23:27:32.0027844 | 23:27:54.1707061 | 78364 / 134380492520046799 | 0 |

均 offline/locked、原B缓存、E TMP、jobs2/debug0/incremental0；环境记录实际 MSVC 14.43.34808 `bin/Hostx86/x64`、SDK 10.0.22621.0。Clippy stderr 有 `proc-macro-error2` 未来不兼容提示；本轮 exit 0。before、两步前后、final六份源码快照各192项逐项相同，a436/双tree和clean/index不变；窗口 completed，锁按已审runner成功路径移除。

[原始证据](B21-a436前置attempt1原始证据/)包括现场顶层25件及prepared四件，共29件229113字节；复制前后源/副双hash与长度一致，复核零差异。[清单](B21-a436前置attempt1原始证据/双hash清单.json) SHA256 `35e65309ce60bfa23a3ff3548c4593ec9dcbef71f18be7512dcff0de92a62226`；[复核](B21-a436前置attempt1原始证据/发布复核.json) SHA256 `dea037c2d020b5e9abcd4b5f955612d7c23f211c7747606da7944c4576aeb875`。不含EXE、target、tmp，原日志绝对路径不改。

此窗口只证明同一a436的check与Clippy；当前candidate完整21矩阵、debug Host、workspace串行test、fmt、UI wasm仍待新窗口。
