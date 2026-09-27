# 员工C：5e1b628 最终五门禁attempt2收口报告

2026-09-28，原员工C/GPT-6-astra，C-R14-01-20260927-E01。依经理正式attempt2放行，唯一C在本人独立树NoManCode/rust-app从check重新串行执行标准build.ps1。固定HEAD **5e1b628abec3154f9a70f22420dce406d691c1a5**，NoManCode **5fc44d1d2c4f65790cab94c376c59b1466e88980**，Rust **e4a5012dea4d2d0f7acb352de496a66f61bb2097**。每步前后及归档再核均三层SHA一致、status/index空；无源码修改。

## 五门禁实际结果

命令统一为 `pwsh -NoProfile -File ./build.ps1 -Action <步骤>`，没有替换成专项目标。

| 步骤 | UTC开始（2026-09-27） | UTC结束 | 原命令exit |
|---|---|---|---:|
| check | 16:58:07.7344369Z | 16:58:13.8628871Z | 0 |
| test | 16:58:14.2511256Z | 17:01:21.6805640Z | 0 |
| fmt | 17:01:22.0581146Z | 17:01:23.6552551Z | 0 |
| clippy | 17:01:24.0345092Z | 17:01:38.3091091Z | 0 |
| wasm-check | 17:01:38.6429696Z | 17:02:06.3118472Z | 0 |

外层runner实际exit0。本attempt无失败/未执行门禁。所有target/TMP/TEMP/cwd/log只在本人E树，既有MSVC只读；具体环境及cargo/rustc1.98.1在meta。未执行付费live/release/push。proc-macro-error2未来Rust兼容警告保留于wasm-check原日志，不修改依赖或隐去警告。

## 完整测试统计

原始test独立统计 **43结果块，564 passed / 0 failed / 1 ignored**。五空helper包含在564内，源码确认环境参数缺失直接return：

- git_worktree::tests::w05_isolated_environment_helper（git_worktree.rs1452）
- provider::network_tests::proxy_environment_child（provider.rs786）
- malicious_environment_child（tests/git_diff.rs655）
- crash_child_write（tests/workspace_crash.rs243）
- crash_child_restore_http（tests/workspace_crash.rs317）

扣除为 **559实际父/普通通过、0失败、1忽略**。子进程内部场景不重复计数，真实父测试名字含child的不扣。此前组件计数未叠加。唯一ignored是 `xpeach_native_stream_and_coding_tool`，付费live未调用。

## W01–W11固定组合覆盖

| 范围 | 本次可核日志/现有测试 |
|---|---|
| W01 | D w01_real_worktree等+HTTP w01_w02整项通过，clean基线创建、branch/admin/common/index与默认Project无权限 |
| W02 | HTTP独立grant/配置/审批写新根、原Session/旧Task绑定通过；旧Engine三回归restore_gate_serializes…、partial_and_unknown_restore_histories…、no_restore_resume…checkpoint_keeps_original_error均ok（日志34/68/40），不再只引用历史5c |
| W03 | B反向准入每入口、同key/同common两个Store竞争、原回放/预约与注册盘点竞争相关lib全部ok；不把每循环场景重复计test |
| W04 | D staged/unstaged/untracked、unborn/bare/非unborn SHA256、reparse/nested/parent替换、已有目标/branch、配置超限与不支持树项相关测试ok |
| W05 | hook/fsmonitor/filter/process/global attrs/inherited env与项目git.exe marker正反控测试ok；D07盘根ctx+隔离cwd反例随同父测试执行，helper空入口已扣 |
| W06 | D非零/超时、输出超限、继承pipe及原handle存活拒绝/killwait正控ok；B取消调用者owned worker继续登记ok。不声称任意后代全灭 |
| W07 | true13非空逐行保全、并发open、marker故障回滚、坏14/FK拒绝lib及旧integration迁移回归通过 |
| W08 | B真实三边界父测试、C Router真实三边界父测试均ok（日志207/206）；每父含三个kill/wait/restart场景，不冒称六个test或生产环境注入 |
| W09 | worktree_cli8项及worktree_http2项完整通过，含严格DTO/auth/跨source、畸形/流读取错误/不一致回执、输出失败与只读POST分类 |
| W10 | D/B诊断与身份/admin异常、反复read无漂移；C clean/dirty PresentMatching及原key回放/旧根保持通过。诊断不自动重试/接纳/释放 |
| W11 | 标准check/test/fmt/clippy/wasm-check全部0，原NEXT05–12完整非live回归同次执行 |

测试范围是Windows本机固定组合；并发约束是同DB Host入口。Git linked worktree不是OS/完整仓库沙箱，unknown预约保留、仅诊断不自动接纳/解除。W08 Provider无派发证据是runs/network_calls/worker观察，不扩成OS socket隔离证明。

## 证据与历史

[六对原件/发布SHA256 manifest](C-R14证据/5e1b628最终门禁attempt2/manifest.json)已逐对重新核验；manifest SHA256 **8A304A96E82584A4C6D9BA521BDD4F22E943EEAA7E4B0918647E6A3D77E61E83**。五日志+一完整meta原件保留在本人.local/logs；发布仅工作树相对化及只读MSVC前缀占位。fmt原日志真实空文件已保留。C轮累计31对（组件18+attempt1三对+C02四对+attempt2六对）。

[每步原命令/UTC/exit/输入与环境](C-R14证据/5e1b628最终门禁attempt2/5e1b628-final-attempt2.json)

935attempt1 check0/test101、首lib199/1/0、后三门禁未执行原样保留；Some9根因仍未证实，本次D08通过不构成PID复用确证。C02 fmt1、D旧错误edition格式结果、前期HTTP/CLI失败及全部旧报告不倒写。CD01固定fc50176 CHANGES_REQUIRED与后续D07复审都保留。

当前源码与构建冻结、clean/index空，无本人持续进程。身份已更新，报告交经理独立核验、验收与main整合；**本报告证明固定候选五门禁通过，不代替经理APPROVED，不宣布完整阶段一完成**。下一第一操作回应经理最终审查所需证据，未有新授权不重跑、不push。
