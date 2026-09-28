# NEXT-15 员工B提交报告0.3：真实Host崩溃模块注册检查点

2026-09-28；员工B / GPT-6-astra；执行ID B-R14-01-20260928-E01保持。此为经理要求的最小依赖注册窗口，不是新任务，不是完整验收。报告0.1、0.2及证据原件保留。

## 本人实接与提交

经理D→B普通merge后的本人实核HEAD为67de9282b198bd7ed310e13a3987455845cab54c；NoManCode tree cafd063db13989fbd6fc0b60e7d7260b6e53a415；Rust tree 1f6682e2254a83610e61f4a76d07166a9ef6e0c1。源码树../nhc-b-host-route-config，分支codex/b/host-route-config，status/index/unmerged为空；实际src/host_configuration_crash_tests.rs存在，B未生成替代stub。

唯一改动为NoManCode/rust-app/src/lib.rs两行：#[cfg(all(test, windows))]与mod host_configuration_crash_tests;。没有编辑D模块或其它源码。git diff --check退出0，git add仅此文件，git commit成功。

- 完整候选：f55cfd8663df04b26eeba31de3c6a6a760a3fa71
- NoManCode tree：0dba4fe3deb896f52a9e12f72db42e4e99c7ae64
- Rust tree：3aa3171157cd145fd117f12a42369be0ef1d8d6a

提交后源码与index干净、无构建及相关测试进程；**持续source/index/no-build冻结供经理将完整组合交D。** 本人未跨树merge、未操作主仓index。冻结读回原始命令与逐命令退出码见证据0.3，所有本次执行Git查询和diff检查均exit0。

## 受测边界与后续授权

本检查点未编译、未执行D测试、Clippy或最终五门禁。经理review0.2确认原f340358候选118个组件通过和37对原/发布证据审查；这是经理限定结论，不证明f55cfd8组合已通过，不是B整任务结项。D817行模块实现及其静态检查贡献仍属于D。

已完整接收补充04：在补充03五文件之外，tests/support/project_fixture.rs以及src/plugin_management.rs仅cfg(test)迁移夹具也唯一授B；合计七文件。仅精确剥离schema16 sidecar/marker及明确新增空表、单例revision=0、marker/版本期望，不改旧逐行/rowid/blob/失败原子和业务断言，不动插件生产逻辑。当前尚未实施，等待经理交D后明确释放B窗口。

下一第一操作：收到经理释放后开展这七处精确夹具与原B范围组件，新增下一报告；最终五门禁继续由C在经理固定完整组合并明确放行后唯一串行执行。全部输出/临时/日志在本人E树；工具链只读，无release、付费live、push、reset、clean、stash或旧树清理。

## 原始与发布证据

[双hash清单](证据0.3/manifest.json)及[逐命令冻结记录](证据0.3/freeze-registration-03.json)已落；原件在本人源码树.local/next15，发布仅工作树路径相对化。清单SHA256：CD5DBBA8138293854AC5DD77C70D6C3E8BD9B44F7D643408089F04FFFD654804。原始/发布SHA256和字节数已从磁盘重新核对一致。
