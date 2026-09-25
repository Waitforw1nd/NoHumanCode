# C-R5-CRASH-SUPPORT：B-R3-01真实崩溃补证

2026-09-26，经理GPT-6派发。用户在员工B GPT-5.6-sol服务余额不足403后明确授权员工改用GPT-6-sol；本支持员工实际模型GPT-6-sol。B由新员工接续原B-R3-01执行ID/在途代码，不重复认领。本支持仅并行补证W07/W08，不接管B实现或重开已验收审批CLI。

## Git与所有权

- 支持开发基线：`dd57adfc9321454651bdcdabb3a081bc95091600`（B中间实现，未验收），独立工作树 `../nhc-c-workspace-crash/`，分支 `codex/c/workspace-crash-tests`；由经理创建。
- 唯一新增源码：`NoManCode/rust-app/tests/workspace_crash.rs`。C仅提交本文件；不修改B实现、workspace_changes测试、其他测试或Cargo/脚本。需要实现hook时先给经理具体方案，不擅增生产后门。
- B唯一负责其原契约源码及workspace_changes测试；本文件从B写权排除。后续B修复稳定提交可由经理协调合入支持分支，先保全当前测试改动；最终由经理串行整合，不能自行合main。
- 共享档案仅niuma/员工C/个人身份认知与提交报告/第五轮，禁止工作树副本。执行ID/真实接收时间/模型/继承改动登记。当前CLI任务已按范围验收，源码冻结于0d4dc9a，不复写其报告运行事实。

## 真停止测试

完整读B第三轮契约及补充1/2、B review1.0、A Workspace第1.2报告，按真实Engine/Store API搭建临时DB/工作区与本机mock，不调用付费Provider。

W07覆盖真实批准write_file：durable prepare/claim后副作用前；文件写完而finish事务尚未提交；finish已提交。必须真正终止runtime或子进程且证明执行者已退出，再重新open/recover。前两不明窗口明确unknown、不自动重做、不从磁盘摘要伪造finished；后者保持finished、无重复副作用。可以复用现有runtime shutdown，遇同步DB/FS无法可靠停止则使用测试二进制子进程并显式kill/wait。

W08覆盖真实restore：durable restore claim后、某文件副作用后outcome提交前、全部完成提交后；恢复后查询稳定restore_id、逐路径事实、unknown/complete，重复POST不重做，任何非空operation封存task写入。至少一个副作用后窗口从真实HTTP恢复入口发起验证安全receipt/重启查询，无法在崩溃时收到响应要明确断连事实，不能伪写HTTP409已收到。

测试要用可证前置条件的屏障/持久标记/SQL故障夹具，有界等待，不能仅sleep猜时序。可以在临时DB安装只用于夹具的触发器/长查询制造确定性提交窗口，必须assert所需行/文件/状态，再终止子进程；清理自建子进程和临时文件，不能杀其他Host。不要把直接SQL造unknown当真实崩溃证明；不得仅drop Arc。

API依赖缺陷交B/经理修复；不为了测试通过弱化契约。先交固定测试SHA及目标场景结果，报告写真实源码组合SHA/文件hash、命令/退出码和未覆盖窗口。定向workspace_crash、fmt/clippy适用检查，最终组合五门禁由B/经理协调，不重复运行发布build/付费live。独立target，既有MSVC进程环境。所有日志相对化归档保留原始字节与前后hash。
