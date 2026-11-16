# full-3 最终独立审计

2026-11-16，员工 D。结论：**固定 full-3 全门禁证据与精确发布副本核验通过**；九个定向 target、全量 Rust、网页及后续静态门禁均有已完成记录。D 仅运行只读 Python/Git 核验，没有执行 Cargo、Host、probe 或真实进程测试，也未操作全局 gate。

原受测 HEAD `0fb205851748fdf25abdf35ca0911a0b24527fd4`，产品 tree `dec9a1e0fc0fbd0a0822dc49b41124015c4202fe`，Rust tree `a4cee432f0a2be0d326519840881d8c7d1b12b26`。实际运行来自经理原监督 session1465；D 从最终 manifest、每步 direct-exit/result、日志及源码快照独立核验，不把经理回报替代原件。

## 结果与计数

|范围|独立核验结果|
|---|---|
|完整计划|15/15 完成，所有步骤 exit0；status=passed、runner_exit_code=0。顺序为 check、debug-bins、九个定向 target、完整 test、fmt、clippy、wasm-check|
|九个定向 target|149 passed / 0 failed / 0 ignored，分别 35、16、11、18、7、16、10、19、17；不叠加到全量总数|
|全量 Rust|915 passed / 0 failed / 45 ignored；66 个顶层汇总，每个均与其声明的 running 数一致|
|全量 lib|437 passed / 0 failed / 44 ignored；另一个 ignored 属于 live target。未执行这些 ignored 项的全量内容|
|网页|六条不同 PASS 记录，6 组通过|
|重复计数|按每个 Cargo Running/Doc-tests 区段取父级最终汇总；full-3 原日志没有额外嵌套子测试汇总（排除数 0），没有人为扣减。九个定向重复执行与组件25均单独报告|

逐 target 数量、原日志行号见 [full-3逐target计数.tsv](full-3逐target计数.tsv)，完整区段和 exe 上下文保存在 [full-3完整证据复核.json](full-3完整证据复核.json)。其中 approval_http 16/0、turn_http 17/0；D 新 HTTP 收束与两 Store 事务竞争候选已由该固定集成树实际覆盖。

## 证据与输入核验

[audit_full_3.py](audit_full_3.py) 完成 **99 项检查，0 差异，exit0**。核验准确的15步计划/参数/1800秒deadline、串行时间顺序、每步 started/result/direct/log 对应、62件 manifest artifact 的完整清单及 hash/长度、最终 manifest 自身稳定、源码前后逐字相同及 source digest、clean 输入、Git双tree、E-only输出与offline/jobs2/debug0/incremental0/单线程环境。

[audit_full3_publication.py](audit_full3_publication.py) 最终版本完成 **305 项检查，0 差异，exit0**，详 [v2结果](full-3发布与最终源码核验-v2.json)：发布的63件文件（62件artifact与manifest）和原件逐字相同，source/published双hash及长度都匹配；发布目录/清单准确，218个源码输入文件与原快照一致，组件12文件仍与final25及full-3快照相等。

|记录|SHA256|
|---|---|
|manifest.json|`041f92f57c806f6733bd24df29ed392d04add47bbd233045ad1770ac9d357640`|
|12-test.log（131070 bytes）|`a6ac6c6366d4d6755398ac1f90f6454d7caf88f5a0ef88154589c03bf1f4c1c2`|
|source-before/after.json|`a9da8478ab33b16f55ab53621d4049759ffc5fae6f4bc9e0dcd7c1fd6846c2d5`|
|publication.json|`2040d5bed4956bfd48e1e54225f7b7b12d054ff7ee7650e832a6076341af92f4`|

v1发布审计的唯一差异为 `frozen-head`，退出码1保留在 [原回执](full-3发布与最终源码核验.json) 及对应 exit 文件。原因是全套已结束后经理整合文档推进 HEAD，首次观察到 `c169307b`；不是受测源码漂移。v2观察 HEAD `db59c9d409367ec48345ad161b2c6c123981a2d3`，明确验证新增差异仅在 niuma、产品双tree仍精确等于受测候选、218输入hash不变。没有重写原受测HEAD或覆盖v1，没有因为docs变化要求重跑产品。

final25的397项核验、冻结双exe hash与12文件源码等价性，以及A14b65ae3 teardown限定静态批准仍见本目录既有报告。矩阵25不与全量915直接相加；组件、生产整合与W3可信成对工件是不同验收范围。

## 边界

runner 明确保留 `gate_retained=true`、`process_tree_cleanup=not_proven_by_this_runner`；原监督会话完成和经理实际清理闭窗属于独立证据，本报告不以PID快照或直接exit0证明整树清理。历史unknown、MSVC首次preflight失败、external-1失败和cl08旧spawn handle缺失保持。cl08监督器exit0与目标Host exit1已在独立审查中区分。

本次不是release/live验收，未安装工具链或开放run_wasm。W3工具仍保持独立，缺Host编译消费者时preflight失败，准备工件不等于运行绑定。
