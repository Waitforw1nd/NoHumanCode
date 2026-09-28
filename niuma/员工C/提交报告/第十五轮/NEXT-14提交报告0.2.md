# NEXT-14 员工C组件专项报告0.2

2026-09-28，C/GPT-6-astra，执行ID C-R15-01-20260928-E01接续。经理仅放行组件check→budget HTTP/CLI→U07，不是最终五门禁。

真实受测HEAD `eb7c1aa0df448428556a30e050d9cc30f63c2b02`、NoManCode `5ed341516026e95c5c8da26067a90032f6c9aa8e`、Rust `7232c48721a508bdf628c44a54a27665f44d9de4`；接收及结束均status/index空。B真实同步Engine/Network/probe及D classifier已合，未用假桩；本次没有源修改或新源提交。

## 真实执行与全部失败

attempt1：build.ps1自动发现MSVC失败，外壳exit1；Cargo check未启动、HTTP/CLI/U07未执行。check重定向文件实际空，不能凭空伪造异常日志；原工具chunk b6f545含真实异常，meta记录Missing Visual C++ x64 build tools。本失败保留。

随后仅只读枚举已安装Visual Studio目录并将VSINSTALLDIR设为当前验证进程环境；未安装、迁移或更新机器工具链。从check重新开始attempt2，全部target/TMP/TEMP/log在本人E树.local。Rust/cargo1.98.1，jobs2、DEV/TEST debug0、incremental0。标准build.ps1未修改，原proc-macro-error2未来兼容提示保留。

| attempt2动作 | 结果 | 边界 |
| --- | --- | --- |
| build.ps1 -Action check | exit0，UTC05:59:06.6994473Z | workspace组件检查，不冒称五门禁 |
| cargo test --locked --test budget_http --test budget_cli -- --nocapture | exit0，UTC06:00:00.2367058Z | CLI8/0/0，HTTP5/0/0，共13，无helper |
| cargo test --locked --lib server::budget_http_crash_tests::u07_real_router_budget_three_boundaries_kill_wait -- --exact --nocapture | exit0，UTC06:00:19.8155969Z | 父1/0/0，213filtered；三phase不是额外三个测试统计 |

U07实际Completion专用before-claim/after-claim/before-finish三停点，父进程真实kill+wait，各两次新Host恢复。Models计数每组1；Completion计数依次0/0/1；before-claim无预算账，after-claim及before-finish恢复unknown并保留R10，重复重开无新增请求或重复扣除。子进程执行同父测试名但未重复计入通过数量。

HTTP实证：配置/CAS/原receipt回放与历史unmanaged；零额度不发completion但保留Models receipt；真实正常usage结算；missing/invalid/overrun及提高配置不解除；鉴权、重复/未知/非法字段、413/415预算envelope。CLI实证：exact PUT与认证GET、全部七call状态、严格不一致DTO拒绝、预算错误白名单、人类边界提示、参数不发请求、stdout关闭及畸形/重复原wire为operation_result_unknown且仅一次请求。

## 证据与未完成

原件只存本人树 `.local/logs/attempt1`、`attempt2`，未改原输出；发布副本只相对化路径，见[C-R15 manifest](C-R15证据/eb7组件专项/manifest.json)。共6组原/发布SHA256逐对实核（attempt1空check/meta两组；attempt2三日志/meta四组）。原工具异常与空日志限制明确保留。未记录真实密钥。

本组件14父测试通过不等于U01–U11联合验收，更不是完整阶段一。B正在补迁移/request_digest/聚合溢出一致性，后续新固定组合需必要复验；最终fmt/clippy/wasm及完整test门禁尚未放行/未执行。人员档案在经理5d2768e归档通知后才解除短冻写本报告。

当前无本人构建/测试进程，源/index clean，source/no-build再次冻结供经理普通merge。下一第一操作收新完整候选SHA/two tree后实核，按经理范围继续组件，不自行最终五门禁或合他人源码。
