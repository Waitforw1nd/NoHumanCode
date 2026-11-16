# 组件 25 矩阵及 full-3 输入等价审查

2026-11-16，员工 D，只读原件与 Git 对象，未运行 Cargo、组件、Host、probe 或任何真实测试进程。

## 原证据

原件：集成树 `.local/lifecycle/component-final-build-1`、`.local/lifecycle/component-final-matrix-1`。发布副本：集成树 `niuma/项目经理/审查记录/2026-11-16生命周期闭环/验证/component-final-matrix-1`。

固定矩阵候选：HEAD `1588cc934e7bdb54a422cb451aa3c5699b458a76`；产品 tree `0eb49b67d2afe21376a0afb711b0e2d8299d94e5`；Rust tree `2643a4274245a8508a03b0f449d1d8554dd01b9a`。

独立脚本 [audit_component_final.py](audit_component_final.py) 完成 **397 项检查，0 失败，退出码 0**；详 [逐项 JSON](component-final-25独立核验.json) 和 [退出码](component-final-25独立核验.exit.txt)。核验范围：

- 原 matrix manifest 全部 82 条记录的长度/SHA256，包括两个冻结 exe；发布清单 82 条的长度/SHA256及与原件逐字比较；原 manifest 发布前后完全相同。未复制 exe/fixture 不等于从 manifest 移除记录，原件仍在 `.local`。
- build manifest 全部 10 条 artifact 记录；两个确定计划分别为 lib `test --offline --locked --lib --no-run --message-format=json` 和 Probe `build --offline --locked --example process_lifetime_probe --message-format=json`。plan/step/direct/result/log 退出码均对应 0、无 terminal_unknown；Cargo `build-finished` 成功，唯一 executable 选择与 selection/inputs 对应。
- 矩阵完整 25 个测试名/顺序与记录的 runner 计划一致，每例明确 1 passed/0 failed/0 ignored、exit0，无 unknown 标记；complete=25、无 incident 文件。
- matrix 的 source-before/after、source-files-before/after 均逐字相同；182 文件哈希清单与 build 的对应源码清单相等；HEAD/双tree经 Git 对象复核，输入 clean/index 空。
- offline/locked、jobs2、debug0、incremental0、单线程测试与 E 盘 target/TMP/TEMP 对应；构建 provenance 明确记录 MSVC 根 `C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.43.34808` 及 SDK 根 `C:\Program Files (x86)\Windows Kits\10`，这是既有只读工具链来源，不是新增输出目录。

|工件|SHA256|
|---|---|
|冻结 lib-test.exe|`67ef0f7cfa31e194c88610530efdfa6d18cf2edb9976d175d77031721d2481c2`|
|冻结 probe.exe|`8a61b8bf2aac1c416c7533c9ee613292000ecc17df70bb650b6c2de120a6dc7a`|
|原 evidence-manifest.json|`75cafb67dd0fb5cf63a0645b0468a30a42173894f80d55ee923883d2cc941bf8`|

冻结副本、构建 selection 和 inputs 的双 exe hash 相等；核验时 target 原路径上的二进制也相同。后续 target 可以重建，证据权威副本是原矩阵 fixed 目录。

## 与 full-3 固定输入连接

[audit_full3_bridge.py](audit_full3_bridge.py) 只读核验退出码 0；详 [full3组件等价与A交叉核验.json](full3组件等价与A交叉核验.json)。full-3 source-before 固定 HEAD `0fb205851748fdf25abdf35ca0911a0b24527fd4`，产品 tree `dec9a1e0fc0fbd0a0822dc49b41124015c4202fe`，Rust tree `a4cee432f0a2be0d326519840881d8c7d1b12b26`，clean=true。

12 文件从矩阵→full-3 输入快照→当前冻结工作区逐项 hash 相等：process_lifetime 实现及测试、Probe、组件 runner、DirectProcess launcher、Cargo.toml/Cargo.lock及 workspace 子 crate Cargo.toml、rust-toolchain.toml、build.ps1、lib.rs 模块接线。组件实现没有新增 crate 外部实现依赖，测试引用同一组件模块；完整列表与每项摘要在 JSON。

因此可把这次 25 例组件结果按源码等价关联至 full-3 固定候选；此结论不是完整 Host/HTTP 或全套门禁通过。full-3 若再改变清单中任一输入、构建配置或模块接线，需要重新评估连接，不能只凭“测试文件无关”延用。D 未根据 PID 快照或直接命令 exit0 宣称树清理；common gate 的开闭及执行会话由经理负责。旧 case16/其他历史 unknown 不改判。
