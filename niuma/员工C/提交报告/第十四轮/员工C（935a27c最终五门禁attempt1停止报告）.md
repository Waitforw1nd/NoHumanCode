# 员工C：935a27c 最终五门禁attempt1停止报告

2026-09-28，原员工C/GPT-6-astra，C-R14-01-20260927-E01。按经理正式放行在本人独立树串行执行原build.ps1，不替换较窄目标。固定HEAD **935a27c43c8661ab83a4a65b7df8c328f4884fe1** / NoManCode **d2ff69a800beb21487a40e8a268544e24c1a549c** / Rust **c48c78b987fb6af018e25f2cf8e37e4ee4f97340**。前后SHA/tree相同、status/index空；源码没有任何修改。

## 结果与停止点

| 步骤 | 精确标准命令 | UTC开始→结束（2026-09-27） | 实际exit |
|---|---|---|---|
| check | pwsh -NoProfile -File ./build.ps1 -Action check | 16:37:20.5488171Z→16:37:26.1929514Z | 0 |
| test | pwsh -NoProfile -File ./build.ps1 -Action test | 16:37:26.5618812Z→16:39:18.8435757Z | 101 |
| fmt | pwsh -NoProfile -File ./build.ps1 -Action fmt | 未执行 | 无 |
| clippy | pwsh -NoProfile -File ./build.ps1 -Action clippy | 未执行 | 无 |
| wasm-check | pwsh -NoProfile -File ./build.ps1 -Action wasm-check | 未执行 | 无 |

外层runner实际exit1；test的build.ps1/cargo实际101，不把外壳1替代原命令结果。标准test为cargo test --workspace --locked，停在第一个lib测试二进制失败，后续integration/workspace/doc targets未运行。无live/release/push、无重跑。所有cwd/target/TMP/TEMP/log在E盘本人树，既有MSVC只读；具体环境及cargo/rustc1.98.1见meta。

唯一失败：git_worktree::tests::w06_nonzero_and_timeout_are_unknown_after_spawn。日志src/git_worktree.rs:2121 assert_pid_absent断言，fixture process41068 survived，left Some(9)，right Some(0)。该helper由pwsh Get-Process -Id查询返回；日志未说明for-loop哪一轮失败，也没有PID创建时间/进程句柄身份证据。本报告**不推断**是真实子进程残存、PID复用或其他竞争；已经交经理/D唯一owner定位，不弱化断言、不以单次重跑掩盖。

## 真实统计与范围

原始日志只有**1个test result块：199 passed / 1 failed / 0 ignored**。其中两个空helper已包含在199：git_worktree::tests::w05_isolated_environment_helper、provider::network_tests::proxy_environment_child；扣除后197实际父/普通通过，1失败。名字含child但执行真实父场景的不扣。无组件数字叠加，无子进程场景重复累计。未执行的付费live不能记为本轮ignored。

| 范围 | attempt1实际情况 |
|---|---|
| W01 | D真实worktree/独立结果诊断lib测试ok；C HTTP integration未运行 |
| W02 | 旧Task封存、Checkpoint原Conflict及B反向优先级lib测试ok；C新Project/Session HTTP整链未运行，先前9c组件结果另存不拼入 |
| W03 | B24项相关lib反向准入/竞争/回放测试名称有ok；不增加到最终199之外 |
| W04/W05 | D配置/树项/脏状态/特殊仓库/alias/reparse及marker/环境/盘根排除所在测试有ok；w05 helper单列 |
| W06 | 非零/超时后的PID存活断言失败；输出超限/继承pipe及owned worker等其他lib测试ok，不据此掩盖失败 |
| W07 | schema13→14及损坏拒绝等lib测试有ok；完整旧integration迁移回归未运行 |
| W08 | B三边界真进程父测试与C Router三边界父测试均有ok；各只计一test |
| W09 | worktree CLI/HTTP integration未运行 |
| W10 | D/B诊断及身份/admin/readonly lib测试有ok；C integration未运行 |
| W11 | check通过，test失败，后三门禁未执行；最终验收未通过 |

## 原件与发布证据

[三对原始/发布SHA256清单](C-R14证据/935a27c最终门禁attempt1/manifest.json)已逐对重新核验。两日志及一个完整meta原件保留；发布副本仅工作树路径相对化和只读MSVC前缀占位，转换列在manifest。未为未执行步骤伪造日志。本C轮组件18对加本次3对，累计21对，不重复统计review文档。

[原命令、UTC、exit及每步前后状态](C-R14证据/935a27c最终门禁attempt1/935a27c-final-attempt1.json)

C构建结束、source/no-build冻结，身份已更新并向经理回报。下一第一操作等待D根因/修订及经理新固定组合；只有新放行后从check完整重起，不在本attempt修源或继续后续步骤。放行不等同APPROVED，main整合由经理决定。

### 给D08定位的固定源码上下文（只读补充）

935的git_worktree.rs1836–1870父测试两例都启动 `pwsh.exe -NoProfile -NonInteractive -Command <script>`，cwd为该fixture的host-data/aux，stdin null、stdout/stderr piped、kill_on_drop、Windows CREATE_NO_WINDOW。第一例写pid及partial后exit7，run deadline3秒；第二例同样写文件后Start-Sleep20秒，run deadline1秒。均mutating=true，随后依次已通过OutcomeUnknown、耗时<7秒、partial内容present断言，读取pid文件再查询。日志未打印例名，不能区分此失败在哪例。

2106–2128的assert_pid_absent另起pwsh运行 `if(Get-Process -Id 41068 -ErrorAction SilentlyContinue) { exit 9 }`，run limit1024/deadline5秒/mutating=false，实收Some(9)。该查询没有创建时间/原进程句柄匹配。本轮未查询或结束PID41068的新现状（当前PID状态不能回写失败时身份），未重跑；已将原件路径与上述上下文发D原owner。
