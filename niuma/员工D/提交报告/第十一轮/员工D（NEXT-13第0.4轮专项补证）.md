# NEXT-13 第0.4轮专项补证与D06/D07修订

员工D GPT-6-astra，2026-09-28；D-R11-01-20260927-E01。Git模式独立树 ../nhc-d-git-worktree / codex/d/git-worktree；D提交本人源码，经理唯一跨树整合。实核经理B→D四参完整基线cf57cfa35aabd8dfb2bcd93c79d5582318a77011 / NoManCode453c069495680db86fbef8833f1a6e614baf26da / Rust56d6a318cf9fcbafb9e576c8f73efcb806ae64e4，接收时clean。

最终本人候选 **574982eb05a914eae95f0e11b31c185166b5547d**；NoManCode **e37f546563add9b30da6b770d85597d1e7e6e5f6**；Rust **51fb90b7b8663f28ce7d986801f372d775e0c516**。仅git_worktree.rs变更，本人源提交。最终status/index空、无构建，已向经理开放整合窗口，源码冻结待review。

N13-D06：Repository枚举大变体改Box<RepositorySnapshot>，B的common_key方法自动Deref兼容；未加allow。N13-D07：within只在根路径无尾分隔符时追加，修复Windows盘根与Unix根边界；普通目录正反例、不同盘负例保留。隔离测试只读取既有E盘根身份，PATH仅本次临时fixture里真实编译的marker git.exe，命令cwd在不同临时目录，断言可信command无法构建(Unavailable)，marker保持缺失。没有在盘根init/注册/写文件。Unix根条件断言在代码中，Windows运行不能冒称Unix动态验证。

## 固定验证

| 日志 | 固定候选/状态 | 结果 |
|---|---|---|
| tests-attempt11 | fc50176，四参真实组合 | 19通过，含1helper |
| tests-attempt12 | 17fe18f，W04/W05补齐 | 20通过，含1helper |
| tests-attempt13 | 5dde657，D06 | 20通过，含1helper |
| tests-attempt14 | 574982e，D07 | 20通过、0失败、0忽略；扣helper为19实质测试，168 filtered out |
| clippy-attempt1/2 | 5dde657 / 574982e | cargo clippy --locked --lib --tests -- -D warnings，均0 |
| fmt-attempt3/4 | 5dde657 / 574982e | 两owned文件rustfmt --check，均0 |

全部原始full HEAD/两tree/status/源hash与起止/exit见JSON。第0.4新增16对文件SHA256见[evidence清单](evidence-0.4-manifest.json)，发布副本路径相对化，原始日志不改。早期所有失败保留于第0.2等清单；[第0.2勘误](第0.2轮勘误与证据限制.md)明确实际24对与attempt7未提交修改候选限制。没有全量test/build/wasm/release/live，未执行最终五门禁。

## W04/W05覆盖矩阵

| 范围 | 动态证据 | 实际边界 |
|---|---|---|
| W04 staged/unstaged/untracked | dirty_expected_head_and_existing_target三状态拒绝；dirty_result_diagnosis三状态PresentMatching、strict后验Dirty、index/content不变 | 创建与诊断语义分离 |
| unborn/bare/SHA256 | special_repository_states，SHA256先真实空commit并证HEAD64位 | 非仅unborn导致失败 |
| 已有目标/已有分支 | dirty_expected_head...与nested_root... | 拒绝且不清理 |
| alias/边界/reparse/root不匹配 | common_key case alias、Windows verbatim、目录后缀正反例、真实junction capture拒绝、真实nested source inspect拒绝、盘根注入隔离 | Unix根未在当前Windows运行 |
| parent替换/OID注入 | 实际rename parent后重建；HEAD~1与全零OID拒绝 | 不自动删除或恢复 |
| unsupported配置/树项 | filter/include/sparse/worktreeConfig/promisor/保护NTFS/submodule；真实120000/160000提交树 | 禁止特性前拒绝 |
| 隐藏dirty/超限 | assume-unchanged实际改文件拒绝；真实>16MiB config不变且无worktree效果 | 输出上限另有W06真实pipe超限 |
| W05 hook/fsmonitor | 原生Git hook run和Git status真实脚本写marker正控；受限创建marker缺失 | 非仅argv比对 |
| filter smudge/process/include | 原生Git cat-file --filters各正控写marker；受限inspect拒绝且marker缺失 | process正控可因无协议退出非0，marker证执行成立 |
| 全局attributes | GIT_CONFIG_GLOBAL指定core.attributesFile及smudge脚本，真实Git cat-file先写marker；隔离受限子进程成功创建而marker缺失 | 与仓库local属性分开 |
| 环境注入 | 子进程隔离GIT_DIR/WORK_TREE/INDEX_FILE/OBJECT_DIRECTORY/CONFIG_COUNT/KEY/VALUE/GLOBAL | 环境污染不进入父测试进程；注入index不存在 |
| Project git.exe | rustc编译真实marker exe，直接执行正控；PATH前置Project目录后受限成功创建且marker缺失 | 另盘根ctx+PATH仅fixture→Unavailable零spawn |

W01真实worktree创建、registered结果只读诊断、detached、同父目录目标通过。W06 nonzero/timeout/overflow/inherited pipe真实进程验证保持OutcomeUnknown，PID不存在及helper kill+wait证据仍保留；不声称OS级任意后代全灭。W10实际删除已创建目标与branch保留admin仍Partial、缺失/替换无关Project在absent/present安全Unavailable、source改变优先IdentityChanged通过。

边界与未代验部分：W05以真实可执行marker反证为主，没有独立socket监听器，不将禁传输静态配置表述为已穷尽所有联网路径。保存origin cross-admin由B新增真实Engine测试；D已实现四参实际admin比较且当前组合编译通过，不冒称D重跑B测试。caller取消/持久claim/三phase kill+wait归B/C，最终五门禁归C；D不替代其验收。没有当前确认待实现D阻断，等待经理对矩阵与联合证据review；如需追加socket专项按明确review继续。

下一第一操作：等待经理整合/固定review，不自行merge或继续改冻结源码。主仓仅本人报告与身份更新，旧树/日志保留。