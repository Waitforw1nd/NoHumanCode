# NEXT-06真实Diff与最小Checkpoint交付

2026-09-27，经理GPT-6。用户要求的[统一契约与任务包](../任务/2026-09-27NEXT-06真实Diff与最小Checkpoint契约.md)已完成并实际派发，原三名GPT-6-astra员工按唯一文件所有权实现、调试与测试；现已完成[联合验收](../审查记录/2026-09-27NEXT-06联合验收.md)，不是停在契约草案。

新增用户能力为真实单文件Git三视图Diff，以及单completed Task的task_before检查点创建、查询和安全恢复，支持CLI/HTTP。Diff读取真实Git对象/磁盘，changes为空仍可审阅；Checkpoint冻结Task首次批准写入前字节/存在性，不以Git commit/reset实现，不声称整个目录快照。创建与恢复复用gate、批准和执行事实分离、先claim后副作用、partial/unknown稳定回执、不自动重试及旧Task封存。

最终受测`9f990648df3308dc7235b028123e5532a35a00f8`，Rust tree `b549de2e18d39c8e69e6607b9f430974895ee4b9`。员工C在D盘唯一串行执行check/test/fmt/clippy/wasm-check，全0；345通过、0失败、1付费live忽略。经理核68对原始/发布证据hash、源码及矩阵，未代写产品或代跑测试。前两轮fmt/Clippy失败和未运行项完整保留；B/D/C最终reviews按范围APPROVED。

范围限制：普通文件字节/存在性、单Task、单Host；不覆盖命令回滚、ACL/ADS/时间戳、任意时点/目录快照。Diff全文件单hunk且敏感正文脱敏，大文件仅有界前缀/长度/mtime复核，不保证恶意ABA或任意OS TOCTOU。具体证据边界详见联合验收。

经理负责最后串行main整合与树一致/九档案保全核对，实际结果以[整合记录](../审查记录/2026-09-27NEXT-06整合记录.md)为准。旧工作树、九继承未提交档案和历史模型贡献保持；未release、付费live、推送。

完整阶段一仍未完成，后续顺序已在契约中明确：**NEXT-07网络授权→NEXT-08多项目配置/权限快照绑定**。下一经理操作是核当前Workspace/Provider/审批网络边界并制定NEXT-07可验收契约，不直接扩Desktop、完整Team/MCP。当前没有向员工派发NEXT-07/08实现任务。
