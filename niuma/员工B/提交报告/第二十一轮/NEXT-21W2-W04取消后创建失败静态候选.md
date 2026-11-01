# NEXT-21W2 W04 取消后创建失败静态候选

2026-11-02，员工B；原第二十一轮 W2/W04 接续。经理已核 796f 诊断：本次 create_after_ticket 实际 late success 并回收，旧 249479 的 create_failed 未复现、错误码未知；C 持动态窗口，本轮仅静态。

基线 `796f8847eaf32bc2494cc2ad962f781f9ba19bfb`、产品 `d1dd57a190246254d476c2f94d1dea638ee91d5f`、Rust `24ecb5761c7acccf49ebd20c8ff9c622e0b980fe`。新提交 `c636d5a9825ee986a1e0a27ef8f286770389bceb`、产品 tree `0e1921d1e7a1ff88ca80888d3d6456bbc019dcfd`、Rust tree `b5c0ce3fb9d406ec56b8711803a87c7a55f6946a`；独立树 clean/index 空。仅修改 `NoManCode/rust-app/src/wasm_worker_process.rs` 和 `wasm_worker_process_tests.rs`，runner exact 名称未变。

仅 `create_after_ticket` 在未观察到 process 时可进入取消后创建拒绝分支：必须依次有 ticket、停点、cleanup_unknown、放行、实际创建 flags、CreateProcess 失败、Stopper join、同一 Job 查询、平台返回；还要求实际非零 Win32 错误、Stopper TerminateJobObject 成功、同一 Job 查询成功且 active0/total<=1，slot 已释放、原 Busy 和 hook 断言通过，无创建成功/未知句柄/resume/提交 IO。缺少或冲突证据打印 `NHC_TERMINAL_UNKNOWN` 并保留现场停窗，不凭 observed=None 通过。真实 coordinator 已返回且 slot 已释放后，只有 observer 仍武装且没有捕获句柄时才原子解除，已消费/已捕获则 unknown 保留，不吞迟到观察。

成功创建路径继续持实际 observer 句柄检查 signal、Job0 和回收；`create_after_ticket` 与 `create_returned_suspended` 均额外禁止 resume ticket/API、实际 resumed、提交及 I/O 阶段。其他阶段仍必须实际取得原 process 观察句柄。slot 在期限内未释放时保留现场而不继续。

同一现有 W04 exact 新增真实 W1 worker 正控（实际策略输出、进程 signal、Job0 和 IO 完成），随后运行测试态正控：仅由内部 atomic hook 给真实 CreateProcessW 加上与 DETACHED_PROCESS 不兼容的 CREATE_NEW_CONSOLE；核实际 flags 同时存在两位、API 真实返回失败和 ERROR_INVALID_PARAMETER (87)，并满足上述 Stopper/Job/协调器/slot 证据。未伪造系统调用返回或错误；若错误/条件不成立则失败或 unknown，不能计作覆盖。此 fixture 是独立参数反例，不代表旧 attempt2 失败根因；原正常 create_after_ticket 和 `create_returned_suspended` 停点仍执行，后者必须真实创建成功。无生产公共可执行路径或环境注入。

静态检查：两 Rust 文件 rustfmt 与 rustfmt --check、提交前后 diff-check 通过；只读本地 windows-sys 0.59 确认 CREATE_NEW_CONSOLE=16、ERROR_INVALID_PARAMETER=87。未编译、运行 cargo/runner、worker/probe/helper、正式测试或非 Windows 检查；真实错误与 fixture 可行性均待经理集中 review 后动态窗口。后续需核 controlled fixture 和两条真实创建分支，完整 W2 仍未验收。

## 后续静态自检：固定最终候选

在 c636d5a9 后新增普通提交 `82a0016ef1d582abc963270b48149e10fca1220f`，补充失败分支对 cancel_io_return 及三路 IO settled 记录的显式禁止，防止仅凭无 submit 记录漏检矛盾现场；没有 amend 或重写历史。最终产品 tree `99ed0add4a89fbee1473a3c511a9972a54148a65`、Rust tree `8fccff04244c3a3b04e120f7bbb2b6e3d5ecdea4`，独立树 clean/index 空。两 Rust 文件 rustfmt/check、相对 796f 的 diff-check 通过；仍未编译或动态。经理应以最终 82a0016e 审两个连续提交。
