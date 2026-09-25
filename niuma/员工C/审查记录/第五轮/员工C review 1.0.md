# C-R5-CRASH-SUPPORT review 1.0 — APPROVED（后副作用补证范围）

2026-09-26，经理 GPT-6。员工 C 实际模型 GPT-6-sol。固定测试提交 `1f2a98101c457008b8e8725716d2d90979b0d0a1`，其实现基底为 B `948e0cccf4f852b265c38a08c6b516470a0476e5`；测试文件 SHA-256 `8D6F41209C2518C1983C8D958EE1E85BA4697AF75A5DA6E03D198D66E57C6E85`。经理完整审阅首份测试及第二份 delta，核对[第1.1报告](<../../提交报告/第五轮/员工C（Workspace真实崩溃补证 第1.1轮报告）.md>)和[命令证据](../../提交报告/第五轮/C-R5证据/清单.md)。

唯一源码新增 tests/workspace_crash.rs，未修改 B 实现。四个场景均真的启动独立测试子进程；父进程确认文件字节与持久化前置事实，kill 后 wait 确认退出，再重开 Store/recover/Engine。两个提交前窗口另要求 DB 写事务占用，触发器只在临时夹具 DB，未增加生产控制入口。两个完成窗口以已提交 finished/complete 作为停止前提。

W07 分别证明审批与 change 的 unknown/finished、tool message/tool_result/finished event 为 0 或 1，没有磁盘摘要推导成功；W08 从真实 HTTP POST 进入恢复，重启后 unknown receipt、逐路径 unknown、change unknown、restorable=false 一致，重复 POST 409 同 restore_id；complete 后重复真实 POST 200 同 receipt，外部新编辑保留。崩溃请求没有收到响应，报告准确区分重启后 409。子进程辅助入口两条普通执行立即返回，故 6/6 代表四个真实父场景加两个入口，不夸大为六个窗口。

C 曾有读取非原子发布 task ID 导致超时的夹具竞态，现已改原子 rename 发布、非空且 DB 行存在校验，并保留子进程诊断。B083eae5 的 pending/unknown 真实失败日志退出码101保留；B948e0cc 上同断言通过。定向6/6、check、fmt、针对本测试的Clippy均为0；经理核证不冒充执行者。

本 APPROVED 只针对 C 负责的四个后副作用/提交后场景。前 FS 的 W07/W08 由 B 私有单元测试屏障补证，完整 W01～W14 与包含 CLI 的组合五门禁仍待完成；Workspace 尚未验收。测试已由经理合入 B 开发组合 `9a247a8ce2440c9b82128f6282dfadc10963032c`，没有把 Workspace 合入 main。源码工作树保留，无发布、付费 live 或 release build。
