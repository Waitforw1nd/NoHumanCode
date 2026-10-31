# NEXT-18 D attempt1 审查与预算断言返工

2026-10-31，项目经理。受测 HEAD `67841d0610374179f056e27e4a0060fc619081ac`，NoManCode `251c949b09ef3a7adb07c32f0834e1e154737d30`，Rust `872776408d4f141d400399bd6605605cc049ed66`。

## 结果与独立核验

all-targets check、CLI build 均 exit0；全 lib exit101，313 passed / 1 failed / 0 ignored。schema 集成、七项历史夹具、Clippy、fmt 未执行。D 已报告窗口释放、结束 clean 与无重型进程。

经理核六份同 hash 前后清单、134 个当前跟踪 Rust 输入、命令元数据及原日志。九个迁移停点和四个管理意图停点的 raw DB/WAL/SHM 哈希、阶段/PID/signal/wait、两次独立恢复输出均已核；只读 immutable 查询十三份 queryable 快照，版本/marker/行数、旧 rowid/负时间与提交前后 receipt 符合各自阶段。详 [原件核验](../../../项目经理/审查记录/2026-10-31NEXT-18-D组件attempt1原件核验.json)。经理未运行产品测试，也未打开原 raw 数据库进行恢复。

这些已通过子项不使组件整体通过，后续完整固定组合仍需验证；旧失败和现场保留。

## D18-R07：Claimed 与 unresolved 的断言错配

唯一失败为 provider_runtime_tests.rs:1443 的 `n18_provider_admission_and_remote_disable_have_a_transaction_order`。该测试先取得 Claimed 回执、未发送网络请求、未 finish/recover，却要求 admission-first 分支 unresolved_count 增加一。

经理核现行 budget.rs 的状态汇总：Claimed 增加 claimed_count 与 held_tokens；Unknown/UsageMissing/UsageInvalid 才增加 unresolved_count。ProviderPermit 没有 Drop 自动将 claim 改为 unknown。相邻 held_tokens 断言已通过，错误属于测试口径，不能据此更改既有生产预算状态机。

授权原 D 仅修本人该测试并增强断言：admission-first claimed_count 增加一、held_tokens 增加100、unresolved_count 不变；disable-first 三者均不变。可按 call_id 补持久 budget 状态核对；保留双向真实屏障、并发、回执 Claimed、in_flight、network_calls 和无发送断言。不改 B/C 源码，不降低并发/延长 timeout，不删除有效断言。

限定格式化和静态差异检查后提交新固定 SHA/两 tree/clean，再由经理审查和放新窗口；原窗口不可原地续跑。发布副本另需按路径约定相对化 MSVC/SDK 前缀，保留原件、旧清单 hash 与转换记录，不影响产品判断。

## 修订复核与 attempt2

经理核 D 固定 `896b690938cb31946acb2c4030de76f58eb13d43`，NoManCode `f687e5f5babcded3585cf56fabc216976434c4f0`，Rust `5e1282deb5df607959c620c25ce440d4cf26e7fd`，status/index/unmerged 空。完整差异仅该测试：按原 call_id 查询持久预算记录并核 Claimed、100 token、同一 receipt；增加 claimed_count 的正反向断言，unresolved_count 保持不变，其余有效断言和屏障不动。静态修订可进入验证，R07 动态闭合待重验。

授权原 D 以新固定候选执行 [原组件窗口](NEXT-18完整组合独立组件窗口.md) 的全部七步，使用独立 attempt2 tmp/log/metadata/每步源码清单和崩溃现场；可复用缓存但重建 CLI，原环境和首失败停止不变，B/C 仍 no-build。原 attempt1 不覆盖。

发布补正已核：16 对文本原/发布哈希、长度与仅路径转换一致，234 现场原文件哈希一致；旧 manifest-v1 hash 保留，新清单为 `e1c8c4dc43db180c00935b580b4c8e2e370f01fd484dad8005b733b0fcc8ad3b`。原件不变，详经理核验 JSON。
