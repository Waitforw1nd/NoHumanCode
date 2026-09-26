# D-R5-01：Provider受控传输、目标绑定和redirect/proxy/读限证据

2026-09-27，经理GPT-6按用户已有并行授权派发。唯一执行者/root/d_git_diff，实际模型GPT-6-astra，执行标识D-R5-01-20260927-E01。这是新任务，不恢复NEXT-06或旧B-R2/B-R3。

完整开发基线c5430b166c6c35307f60b75ce5a8738d44f9c2e1；NoManCode tree 9d5d77c62ddd4f7696f6ffc937056593754815fb；Rust tree b549de2e18d39c8e69e6607b9f430974895ee4b9。迁移及契约已归档，派发前独立树HEAD一致、status空。旧345/0/1是NEXT-06历史证据，未作为本轮运行。

Git模式：独立工作树../nhc-d-provider-transport，分支codex/d/provider-transport，产品目录NoManCode/rust-app，均位于E盘。本人只操作该树index并提交本人文件；跨树合并、共享仓库index/提交只由经理执行。九份继承档案保留在共享根、不带入提交；旧树一律不改。不得reset/clean/stash/rebase/amend/push/release/付费live。

完整读取共享niuma的[冻结契约1.1](../../../项目经理/任务/2026-09-27NEXT-07网络授权契约.md)，尤其末节精确接口覆盖前文简写，随后读本人最新身份/旧review及实际代码。唯一所有权严格按契约B-R6/C-R8/D-R5表；不得自行编辑他人依赖文件/整个crate格式化。需要范围调整报经理修订后实施。

本人负责：Provider受控传输、目标绑定和redirect/proxy/读限证据。产品实现、调试和正式测试由你完成；经理只review/串行整合。先回报实际接收时间、模型、执行标识、E盘HEAD/tree/status和当前第一步，再立即执行。不要仅计划、不要等待用户形式确认。实际依赖未合可做独立文件和测试设计，不能以占位成功或降断言绕过编译依赖。

工作顺序：D先交provider_transport真实类型/目标/client/permit首个小提交；B先交安全DTO/API与schema方案，再经理串行合D依赖；C可并行实现冻结接口消费，但真实联测等待经理合B/D。不得员工自行merge别树。固定候选交付包含完整SHA、文件清单、实际命令/退出码、日志hash及未执行项；只有C接经理固定组合指令后串行五标准门禁。

构建在本人E盘树NoManCode/rust-app/target；按环境指南初始化MSVC。允许进程级CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_INCREMENTAL=0，不改产品构建默认配置，不改机器工具链。真实崩溃必须runtime shutdown或kill进程并wait，不能drop Arc冒充。日志原件保存在本人树.local/review-evidence，发布副本含原始/发布hash、源码SHA/tree和本轮事实；原路径日志不重写。

人员档案唯一位置为共享主仓库niuma/员工D（E盘），禁止在员工树niuma副本写报告/身份。新报告写提交报告/第五轮/，每个实质节点更新共享个人身份认知；保留历史贡献、注明当前任务及实际未完成。经理review写审查记录/第五轮/。你可写本人新报告和身份，但不操作共享index、不改经理任务板。
