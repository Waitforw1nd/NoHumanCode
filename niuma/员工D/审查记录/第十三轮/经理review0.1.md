# NEXT-15 D真实输入首候选经理review0.1

日期：2026-09-28；审查人：项目经理GPT-6；执行ID：D-R13-01-20260928-E01；实现/组件执行者：员工D GPT-6-astra。

## 结论与范围

**APPROVED（仅真实安全输入组件，允许作为B/C依赖串行整合）**。未发现阻断该组件依赖整合的问题。本结论不是D完整任务结项、I04/I08/I09全部通过、NEXT-15验收或最终五门禁通过。

固定候选3675e3afeebb7c6ce73200a888588775bc55e3e9；NoManCode tree d0a79d25547e01d11d2b5117b6739423353292b5；Rust tree 44af58b76b067b7e34a29689ebe9c9f720a52309。独立树../nhc-d-credential-input，分支codex/d/credential-input。仅src/credential_input.rs与Cargo.toml必要Win32_System_Console feature两授权源码变化；未改产品Cargo.lock/lib/B/C核心源码。

## 源码审查

已完整核真实输入模块：Prompt使用CONIN$/CONOUT$与ReadConsoleInputW，禁回显并以RAII恢复原模式，不退化为回显输入；取消、不可用、非法输入仅固定安全错误。Stdin只接重定向管道/文件，读取有界，严格UTF-8及长度/空白/控制字符/多行约束；不把交互TTY当作安全stdin。SecretInput无Debug/Display/Clone/Serialize，消费后String生命周期由调用者负责；尽力内存擦除不提升为操作系统无副本保证。私有隐藏console用于组件测试，不操作用户现有控制台。

## 证据核验

报告为[员工报告0.1](../../提交报告/第十三轮/NEXT-15提交报告0.1.md)。经理独立核manifest恰18份不同原件/发布配对，逐份长度与双SHA256一致；其中2份明确标注为工具结果转录，不冒充原始stderr。详情见[经理核验JSON](../../../项目经理/审查记录/2026-09-28NEXT-15-D输入组件证据核验0.1.json)。

隔离组件crate的lib.path直接指向真实生产文件，windows-sys 0.59.0与产品锁一致，不是复制实现或stub。attempt3原始Cargo统计10 passed / 0 failed / 0 ignored，其中2项为空辅助入口，实质8项顶层检查；私有console子进程执行30组Prompt与2次panic恢复，另有8次pipe/detached子进程。限定Clippy all-targets -D warnings退出0、单文件edition2024格式检查退出0；均不是完整peachsh或最终五门禁。

attempt3于2026-09-28T14:13:52.9978797Z至14:13:55.5922478Z执行；限定Clippy于14:14:27.1103196Z至14:14:28.7394855Z执行。运行时HEAD仍为开发基线，前后生产文件hash与固定候选对应字节一致；不改写为提交后新跑。环境发现失败、attempt2的9通过/1失败及准备工具失败均原样保留。经理本轮未运行产品测试。

## 尚未完成与下一步

1. B唯一导出真实credential_input模块；由经理在当前明确clean/no-build窗口整合，不能让C/D自行merge或以缺模块/stub冒充可编译。
2. B/C真实依赖整合后，D继续DPAPI、DB/WAL/日志无明文或裸秘密摘要、环境覆盖、恶意响应/未知结果反证。
3. I09必须是生产Host路径的事务commit前、commit后响应前真实子进程kill+wait与两次重开核全旧/全新及唯一receipt、Provider零调用；输入夹具超时kill不是Host崩溃证据。
4. cfg(test)停点与真实Host测试子进程接线由经理/B先确定，不允许D越权修改lib/Engine/Store，也不得暴露生产故障控制面。具体后续以经理契约补录为准。

D于2026-09-28T14:17:10.3595223Z本人声明source/status/index clean、no-build及本轮档案冻结；经理复核固定HEAD、两源码tree、status/index为空。B当前仍有工作修改且未持续冻结，不能合入B；C明确提供接收真实依赖的持续冻结，整合前仍须再次核实际状态。员工合后回执须本人给出，经理不代收。
