# NEXT-21A A0 attempt1首次启动失败审查

2026-11-01，项目经理。固定候选 `0fe116ab95235fbacc62dd818f12a81504ae5807` / 产品 `8e7cffd8e21c09cf7150c02257ae42dae5436ac4` / Rust `cd4d4256f49ade1135b88810bac51b4491c36865`。当前A0未通过，不是隔离拒绝成功。

北京时间06:50:36至06:51:17，workspace all-targets check与显式probe构建均exit0，S01单项exit101（0通过/1失败/3过滤），其余三项、fmt与Clippy未执行。原始记录在 `../nhc-b-external-runtime-isolation/NoManCode/rust-app/.local/next21a-b-0fe116ab-attempt1/`。

S01失败事实：两个fixture ACL命令成功，封装事件仅有profile_created和before_create，均无PID；`CreateProcessW`返回失败，错误为 `create_lpac_process / Some(203) / cleanup_unknown:false`。probe stdout/stderr为空，未到token、Job或权限能力断言。不能推断LPAC已启动，不能把启动失败算作受限访问被拒绝。

经理核五份源码清单相同、185实际输入与记录一致，逐项Git过滤后blob均同固定候选，37个现场文件已有hash快照；当前probe与构建记录SHA256同为 `1b75e7446ed7c17a84df6c788570d0c32a32d27e336cee7fff55cdbd1a8b0fe4`。详 [核验清单](../../../项目经理/审查记录/2026-11-01NEXT-21A-A0attempt1核验.json)。此证据不证明profile删除实际成功，错误返回路径没有该回执。

经理首次web请求官方错误码页面返回空内容，已立即向B更正“本轮实读”的错误措辞；随后使用urllib实际读取Microsoft官方 `https://learn.microsoft.com/en-us/windows/win32/debug/system-error-codes--0-499-`，确认203名称为ERROR_ENVVAR_NOT_FOUND。它仅说明错误类别，不证明具体缺失变量。当前child环境只显式设置SystemRoot，根因仍由实际修正对照验证，不作猜测。

原B继续同一任务定位普通启动问题，沿首次动态授权可在六路径内新提交/new attempt修复；保持旧现场、最小显式环境和LPAC/Job/权限断言。不得继承全量宿主环境或凭据，不得降低隔离来绕过错误。涉及权限模型改变则另交经理裁决。C/D冻结，main产品仍NEXT-20；没有将此失败候选整合主线。下一第一操作核B修正差异及新attempt首结果。
