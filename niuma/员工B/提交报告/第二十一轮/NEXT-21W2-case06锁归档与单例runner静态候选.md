# NEXT-21W2 case06 闩锁归档与单例 runner 静态候选

2026-11-01，员工B。经理审收 `44689e1970d7064d2c1c65091c29f773ef6e79e6` 测试态取证增量；显式 Stopper join 后查询改变观测时序，不能用新现场追认 attempt2 旧错误原因。

## 旧闩锁限定处置

经理接受的依据：runner 持原 test 对象记录 PID 77680 / creation FILETIME `134380213993600100` exit 101；前置 control 有本次 worker PID77696 的 process wait signal 与 Job active0；失败点在 `observed.expect` 之前，`reached/free/hook_ok` 与 Busy 断言已执行通过。注入阶段 ticket 放行后 CreateProcess 返回零、没有成功返回 process 对象，也未进入该注入阶段的成功挂起/resume 路径；当前 PID、父子及 control PID 快照为空。该限定处置不证明旧注入 Job 的 active/total 或 W04 全部通过；没有观察到当次 Win32 错误码。

再次核 gate 的 PID、creation、nonce `ebabddf6f5ee44d7b53aea0f761abd7c`、exit101 与 incident 一致，当前数字 PID/父子/control 快照空，无相反现场。原 gate 原样移动至 `../nhc-b-wasm-worker-process/NoManCode/rust-app/.local/b21-w2-full-a-249479ad-attempt2-audit/case06-gate.original.json`；移动前后 SHA256 均为 `69be14f47b5914c6595f9fd032f4805b2f2119054b540859833a0051f4eeb14b`。依据、时间、incident hash 与当前快照见同目录 `gate-archive.json`。保留原 incident、场景和全部日志；无猜杀/重跑。

[微软 AssignProcessToJobObject 文档](https://learn.microsoft.com/en-us/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject)说明目标 Job 或父 Job 正在终止时分配会失败。这是待诊断的可能解释，旧 attempt 缺少错误码和该 Job 收束实录，不能据此认定根因。

## runner 静态增量

基线 `44689e1970d7064d2c1c65091c29f773ef6e79e6`、产品 `20fa7d20275d2bb532966b5629dfa60d9956d39b`、Rust `79776100435538b70099ce5891b693e30f0ed058`。新提交 `796f8847eaf32bc2494cc2ad962f781f9ba19bfb`，产品 tree `d1dd57a190246254d476c2f94d1dea638ee91d5f`，Rust tree `24ecb5761c7acccf49ebd20c8ff9c622e0b980fe`；独立树 clean/index 空。相对基线仅 `NoManCode/rust-app/tests/support/wasm_worker_process/run-controlled.ps1`。

`-ExactTest` 可选，构建前只接受原20个映射名之一，大小写精确匹配，helper 和任意名拒绝；不提供时仍选择全部20。无论单例/全部都先枚举并核20项与3 helper 的完整映射。`selection.json` 保存 single/full、选中名与计数，输出只称选中范围，不声称全 W01–W06。`build-meta.json` 与 `test-build-meta.json` 分别记录命令、target、开始/结束、真实 `$LASTEXITCODE`；未成功启动时为 started/null，不伪写通过。其余 source/hash/首败停窗规则沿用。

PowerShell parser 和 diff-check 通过；静态清单计数20+3，选择检查在 cargo build 前，完整映射校验在选中用例循环前。未运行 runner、Add-Type、cargo、测试、helper、worker 或 probe；单例逻辑及 meta 动态效果均待经理审查后签窗验证。下一步经理 review 此增量，再决定只针对 case06 的诊断窗口；此处未宣称 W04 或 W2 验收。
