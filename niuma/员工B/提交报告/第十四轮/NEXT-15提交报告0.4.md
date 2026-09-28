# NEXT-15 员工B提交报告0.4：七处历史迁移夹具完成

2026-09-28；员工B / GPT-6-astra；执行ID B-R14-01-20260928-E01保持。经理已将注册检查点交D并明确释放B窗口，本人按冻结补充03/04完成授权七文件。此为组件交付待审，不自行APPROVED，不代表最终五门禁或NEXT-15整体验收。

## 固定候选与持续冻结

工作树 ../nhc-b-host-route-config，分支codex/b/host-route-config；本次起点f55cfd8663df04b26eeba31de3c6a6a760a3fa71，NoManCode 0dba4fe3deb896f52a9e12f72db42e4e99c7ae64 / Rust 3aa3171157cd145fd117f12a42369be0ef1d8d6a。起步source/index均空，经理依赖整合事实与注册边界见[报告0.3](NEXT-15提交报告0.3.md)。

本次唯一新提交：0f731efd993ef6ab0a7a931df8e0d41e3e377bc5，提交说明test(migrations): preserve historical fixtures through schema 16。

- 完整HEAD：0f731efd993ef6ab0a7a931df8e0d41e3e377bc5
- NoManCode tree：0dc65d6d27f7da1ea8114b615f11c71b510bd014
- Rust tree：4f923626851e40859080eb80b1e095bcf809e1bb

2026-09-28T15:21:30.0903539Z本人再次实核source/index/unmerged为空，无cargo/rustc/clippy/rustfmt/link/相关测试进程。**现在持续source/index/no-build冻结供经理review与整合；未经明确接续不继续改源码或重跑。** [冻结记录](证据0.4/freeze-fixtures-04.json)保存逐条Git原命令及退出码，均exit0。人员报告落盘不解除源码冻结。

## 唯一范围与精确迁移变化

相对NoManCode/rust-app仅七文件：tests/approval_gate.rs、checkpoint.rs、project_context.rs、project_migration.rs、workspace_changes.rs、tests/support/project_fixture.rs，以及src/plugin_management.rs的cfg(test)部分。没有修改其它owner文件或插件生产逻辑，没有新增/删除测试。

- 所有已定位的旧schema降级入口明确DROP host_config_requests、host_config_state并删除host-route-configuration-v1 marker，再沿用原预算/工作树/插件/Project等降级链。不是仅改user_version或忽略新表。
- 原最终schema版本断言从15精确更新为16，包括并发open合法结果和坏库不修复的版本断言；保留原历史目标6/7/8/9/10/11/12及其失败回滚版本，没有全局数字替换。
- Checkpoint完整快照助手增加明确空host_config_requests与精确host_config_state=(id1,revision0)。原SELECT *列形状不额外插入rowid，不扩大表过滤，旧值/顺序比较保持。
- 插件快照原SELECT rowid,*保持，新单例精确三列=(rowid1,id1,revision0)，总表数只增加两表、marker数只增加一个；显式verify_schema_16及marker唯一性。既有旧行包含性/完整相等、原rowid/blob、独立CAS和失败原子断言未删。
- Project旧授权迁移连续open后显式确认精确单例、空receipt表和一个Host marker；旧网络摘要/grant/receipt/ciphertext逐值与回放断言保持。
- 静态记录确认差异恰为七文件；插件cfg(test)前生产段与f55cfd8文本一致（比较仅规范CRLF/LF）。初次直接rustfmt未指定style edition产生排版漂移，提交前以项目2024风格精确复原并核生产段，未在漂移版本编译测试或提交。

## 固定干净候选的实际验证

以下四项均针对0f731efd完整提交和上列两tree；原metadata记录空status/index。工作目录为本人源码树NoManCode/rust-app；TMP/TEMP/target/日志均本人E树.local/next15，工具链只读，offline/locked、debug0/incremental0/jobs2。

| 证据标签 | 原始结果 | UTC区间（2026-09-28） |
| --- | --- | --- |
| fixture-integration-01 | exit0；approval_gate35、checkpoint15、network_authorization16、project_context7、project_migration3、workspace_changes21，共97通过/0失败/0忽略 | 15:17:58.5796485Z至15:19:04.4984696Z |
| fixture-plugin-01 | exit0；plugin_management限定7通过/0失败/0忽略，230 filtered | 15:19:34.2584488Z至15:19:57.0053392Z |
| fixture-static-01 | exit0；七文件rustfmt check、提交diff check及插件生产段一致性均通过 | 15:18:36.3096155Z至15:18:37.3402775Z |
| fixture-clippy-01 | exit0；库与选定六个集成目标，-D warnings | 15:20:12.4110198Z至15:20:27.9627785Z |

本次104个不同组件用例通过。旧f340358的118项仍只归属其原受测版本，不相加冒称本组合222项或全仓统计。插件lib-test确已编译包含真实C/D模块的测试库，但运行过滤只有plugin_management::，不冒称D崩溃/安全测试通过。

原始命令（各次原metadata也完整保留）：

    cargo test --offline --locked --test approval_gate --test checkpoint --test project_context --test project_migration --test workspace_changes --test network_authorization -- --nocapture
    cargo test --offline --locked --lib plugin_management:: -- --nocapture
    rustfmt --edition 2024 --style-edition 2024 --check tests/approval_gate.rs tests/checkpoint.rs tests/project_context.rs tests/project_migration.rs tests/workspace_changes.rs tests/support/project_fixture.rs src/plugin_management.rs
    cargo clippy --offline --locked --lib --test approval_gate --test checkpoint --test project_context --test project_migration --test workspace_changes --test network_authorization -- -D warnings
    git diff --check f55cfd8663df04b26eeba31de3c6a6a760a3fa71 HEAD

这些是授权限定组件和静态检查，不是最终五门禁。当前批次没有测试/Clippy失败；旧批次全部失败仍按报告0.1/0.2原样保留。本批static的命令包装首次Select-Object显示null只是OrderedDictionary展示问题，保存的JSON字段完整；已重新读取JSON核验head/命令/退出码/格式及production比较均正确，不以null包装输出代替原证据。

## 原始与发布证据

[证据0.4清单](证据0.4/manifest.json)含11组原始/发布映射：四项各log+metadata共8份，静态diff-check原log、实际runner和最终冻结记录各1份。每项记录原始/发布SHA256及字节长度，已重新读盘逐项核对11/11一致，所有发布JSON可解析。

清单SHA256：7009F4E20EA854239F6DEB3683102DC6A2F49CDEC896ADAF5D56C19EF541F3F8。原件保留在 ../nhc-b-host-route-config/NoManCode/rust-app/.local/next15/，发布副本仅路径相对化与UTF-8规范化。空格式/diff日志是工具成功时无输出，不能因零字节误记未执行；退出码和命令在metadata。0.1至0.3报告和证据不修改。

## 未执行与交接边界

本次七文件夹具与授权限定回归完成，无已知本批实现阻塞。仍待经理review、固定联合候选以及C/D各自完整组件与最终组合验证。B没有运行D安全/真实Host kill+wait，也没有运行C专属最终五门禁；任何后续其它树修订不自动继承本次受测结论。

下一第一操作：经理读取0.4、核固定SHA和持续冻结后串行整合；本人仅响应明确review返工或新依赖窗口。所有跨树merge、主仓index和档案提交仍仅经理；无release、付费live、push、reset、clean、stash、安装更新或旧树清理。本人保持员工B角色和同执行ID，结果在当前对话及主仓本人档案交付，不自动跨对话发消息。

