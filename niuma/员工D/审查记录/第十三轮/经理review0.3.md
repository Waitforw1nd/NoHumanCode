# NEXT-15 D真实崩溃与安全组件经理review0.3

2026-09-28；项目经理GPT-6；D-R13-01-20260928-E01，员工GPT-6-astra保持。

结论：APPROVED（限定输入/I04/I08/I09及非空保护组件）。固定候选cb5e20d9cac1dcb2747eadd3e516a9945b539ab7，NoManCode 7715c222077ce1c0bdbc084faf5e32e704d23b47，Rust ce3c7c470d9eee59860dc703f8570f22cb66b2cd。不是D整任务、完整NEXT-15或最终五门禁通过。

经理已读报告0.3全部内容，完整审阅原安全测试及扩展/最终增量、真实崩溃模块与扫描时机修复。固定五批命令均exit0；Cargo原始输入10、I09 3、安全7，共20通过/0失败/0忽略，其中4辅助入口、16实质顶层测试；D限定格式/Clippy通过，不代替workspace门禁。前后HEAD及两tree、clean状态、每批六源文件SHA256和实际Git blob独立一致。

262对原件/发布文件SHA256全部独立一致，232对原样、30对只替换已知仓库/工具链路径；manifest B7031B5847BFBC4FF1539A531666CAAF737DBCB667C0A15C770E4349373BFECB。30个最终fixture根313个原始文件的长度/hash全部核验，DB/WAL/SHM保留原E树，不改写为文本。旧失败/0.1/0.2保持，发布runner只是审计副本。

I09原始lifecycle.jsonl逐事件核验：两阶段原PUT在停点未完成；原child句柄kill+wait，seed/原Host/两次restart共8个Host均reaped。before_commit原PID45692，双重启revision2/receipt0；after_commit原PID35252，双重启revision3/receipt1；四次恢复Provider均0。两个原始lifecycle哈希与报告一致。扫描原文件放在kill+wait之后、新SQLite扫描句柄之前，保留原SQL快照和秘密/裸摘要/指纹断言；未跳过WAL或吞文件锁异常。

安全矩阵实证含22类非法pipe组合、22次真实commit后恶意/损坏响应human/JSON、实际在途PUT的lookup404、同key显式重放、DETACHED_PROCESS无CONIN$且不读pipe、env隔离与DPAPI命名空间/坏密文失败。旧Task/双Project授权及真实unknown预算held100/unresolved1/spent5保持，metadata不改grant、换key只stale；Provider基线3=1 models+2 completions，不把配置误计为新增调用。CLI timeout只kill+wait自有child并排空两流。

原Windows锁失败、404/探测计数预期错误、无控制台fixture错误、格式器及runner工具错误全部保留；现未发现产品实现阻断。测试覆盖同Windows用户DPAPI，不外推跨用户/OS故障或任意未知元数据秘密通用scrub。经理没有执行产品测试。

证据：[经理独立审计](../../../项目经理/审查记录/2026-09-28NEXT-15-D安全崩溃证据核验0.3.json)、[报告0.3](../../提交报告/第十三轮/NEXT-15提交报告0.3.md)。D持续source/index clean/no-build；完成D→B→C后由C对完整组合独占最终门禁，仍须最终联合验收与main整合才结项。
