# 员工D：真实 Git Diff 第1.0轮报告

2026-09-27，员工D / GPT-6-astra。任务 `D-R4-01`，执行标识 `D-R4-01-20260927-N06`。依据[第四轮提示词](../../任务/第四轮/员工D提示词.md)及[统一契约](../../../项目经理/任务/2026-09-27NEXT-06真实Diff与最小Checkpoint契约.md)，提交待审，不自行判定 NEXT-06 已验收。

## 基线、交接与固定候选

实际接收核对时间 `2026-09-27T01:33:42+08:00`；基线 `7a4a001d4296a26086d5196997ae80ac88e1c22b`，独立树 `../nhc-d-git-diff`，分支 `codex/d/git-diff`，开工 HEAD 与基线一致、status 空。本人只提交独立树源码，经理为串行整合人及共享 index 唯一操作者；人员档案仅写共享员工D目录。旧任务/旧树/九继承档案未改。

- 首个固定实现 `a5c2dd1e4979c88e7936bc90e572b8075753cad6`，Rust tree `81ee9a89c23ef2d7fab7e6f9851abf7ae6465f8b`：D盘定向15/15、check/fmt/Clippy全部0。
- 经理离线边界审查后新增 `GIT_ALLOW_PROTOCOL=""` 与自定义 helper 的有效控制/拒绝反例，阶段候选 `0def34f75564437371f399b19330a206e06f8fd5`，Rust tree `edc94625763bad572d3af07862afbd0ea7f97736`。定向该反例1/1、fmt及Clippy0；没有将旧15项结果冒称新SHA完整重跑。
- 最终候选 **`1b80486f682d978162e056d4a8605f4074187c41`**，Rust tree **`b139550f6d7030c7edf66b9cd7809c7a77e26e8e`**。只读自查发现B导出模块后lib-test构建可能将集成测试专用helper判为未使用；经理授权将其 `#[cfg(test)] pub(crate)` 改为 `pub`，生产构建无此函数、生产API不变。必要check/定向Clippy/fmt全部0，未重复矩阵。
- `2026-09-27T02:09:59+08:00` 最终提交后 `git status --short` 为空，无本人后台构建；源码明确冻结。仅继续完成本报告和证据归档，后续源码变更需经理明确 review。

相对基线恰好新增 `NoManCode/rust-app/src/git_diff.rs`、`NoManCode/rust-app/tests/git_diff.rs`。lib.rs 仍由B唯一写；无Cargo/lock、schema、Engine、HTTP、CLI、Provider、审批或恢复实现改动。完整源码SHA-256及每份原始/发布日志双hash见[manifest](证据/manifest.json)。

## 实现与公开契约

生产入口为 `read_git_diff(root, GitDiffRequest) -> Result<GitDiff, GitDiffError>`，异步。`GitDiffView` 三视图、`GitDiffStatus` 五状态均 serde snake_case；请求拒绝未知字段；五种 `GitDiffError` 无不安全payload并实现静态 Display/Error。B已确认 Engine 接口，C已确认反斜线规范为 `/`、InvalidPath优先于非Git根的Unavailable。

读取真实本地HEAD、单路径index mode/OID及原始blob/工作文件字节；不查询changes、Store或Provider。HEAD→index 不混入磁盘字节，index→work及HEAD→work读取真实磁盘；未跟踪/删除与unborn基准按存在性处理，rename按两个路径显示。禁用Git filter/textconv/external diff；补丁由真实字节生成全文件单hunk，线性成本、保存CRLF及无末尾换行标记，不声称最小编辑hunk或rename识别。

路径先走 safe_metadata_path 和 workspace::resolve；拒绝穿越/凭据/.git/链接及reparse，额外拒绝单独的范围通配符 `*`。仅接受root自身为Git top-level的普通仓库或合法linked worktree；拒绝父仓库隐式发现、bare及嵌套范围。index unmerged、Git symlink、gitlink不当作普通字节。仅HEAD/index均无对象的未跟踪路径做ignore拒绝；staged deletion即便随后ignore仍展示HEAD删除。

Git可执行文件从绝对PATH条目解析，排除仓库内候选，避免Windows当前目录git.exe抢先执行。子进程环境白名单重建，清除Git重定向/config/SSH/pager继承变量；关闭fsmonitor、外部diff、hook、凭据助手、optional locks、replace objects与lazy fetch。空 `GIT_ALLOW_PROTOCOL` 白名单及命令级协议限制拒绝所有transport（含自定义helper），不只依赖当前Git识别lazy-fetch环境。参数数组调用，不经shell；Git stderr只作有界采集，不进入错误/响应。

stdout按命令用途有界、stderr≤8192字节，每个Git进程10秒超时；输出溢出或超时都kill并wait回收。单侧正文≤256KiB、patch≤512KiB，序列化DTO预留4096字节包装后仍≤CLI的1MiB；超限返回too_large而非截断。NUL/非UTF8返回binary无正文；原始字节SHA-256不使用脱敏后内容计算。

调用现有 secrets::redact_persisted 字符串规则，在加unified diff的 `+/-` 前脱敏，避免减号遮住token边界；没有用会折叠空白/截断到400字的诊断scrub formatter破坏补丁。敏感内容改变时redacted=true，补丁只供审阅。已知规则不能识别任意未知秘密。

查询返回前复核HEAD、该路径index mode/OID，重新resolve/root验证及相关工作文件；支持正文范围内逐字节双读，有变化Conflict。Host并发gate由B接入。本组件不承诺抵抗恶意精确ABA或任意OS TOCTOU。

## 验收矩阵与证据边界

| 契约 | 实际测试/实现证据 | 结果与边界 |
| --- | --- | --- |
| G01 | `three_views_read_real_objects_and_preserve_crlf_and_eof`：同文件HEAD/index/work各异、三视图实际增删、完整OID、CRLF、EOF标记；原HEAD/index/work字节查询后不变 | a5c2dd1 D盘通过；模块无需Store/changes即可diff，Engine任务入口由B/C联测 |
| G02 | unborn/untracked/ignored/missing/empty；deletion/rename及ignored staged deletion；root/bare/父根/嵌套/linked worktree；unmerged/gitlink/Git symlink | a5c2dd1 D盘通过；真实临时Git仓库和真实index，merge夹具已确认形成冲突 |
| G03 | 中文空格、前导减号、方括号文件的未跟踪及已提交字节；敏感/穿越/pathspec拒绝；外部文件及目录symlink哨兵 | a5c2dd1 D盘通过；Windows目录symlink为实际reparse；未单独使用mklink /J构造junction，统一reparse属性拒绝分支复用workspace |
| G04 边界/秘密/环境 | binary、>256KiB、patch上限、JSON转义膨胀；stdout溢出和超时进程；外部diff/textconv/filter/fsmonitor配置哨兵；隔离子进程恶意GIT_DIR/WORK_TREE/INDEX/OBJECT/CONFIG；原始digest及敏感行首脱敏 | a5c2dd1 D盘通过。15个顶层测试含1个仅供隔离子进程运行的helper：常规运行该helper无环境时直接返回，父测试会实际单独启动它；不是15个完全独立业务场景 |
| G04 无网络补强 | `absent_promisor_object_is_unavailable_without_network_or_writes`：缺失对象、promisor、显式http allow、本地监听零连接且index/工作文件不变；最终扩展自定义nhcfixture helper有效执行控制与空白名单零哨兵拒绝 | 原反例a5通过；扩展反例0def34f定向1/1通过，测试helper只是本地写哨兵/退出，不访问网络 |
| G05 | `deterministic_head_index_and_work_changes_return_conflict`：生产读取后/复核前确定性async屏障，分别真实commit改变HEAD、git add改变index后还原工作字节、直接改变工作字节 | a5c2dd1 D盘三个分支通过，均精确Conflict；不是靠偶发竞争 |

经理已明确允许的大文件范围：>256KiB工作文件仅读取256KiB+1前缀，重核prefix hash、len、mtime后返回too_large，相关digest可为null（未计算，不表示不存在）。不承诺检测同时保持前缀/长度/mtime的尾部外改；避免为了已拒绝正文无限读取。普通可返回正文文件仍完整双读比较。全文件单hunk及该限制已同步统一契约。

## 实际执行、失败与环境变化

Rust工作目录为独立树 `NoManCode/rust-app`。调用 `build.ps1 -Action check` 配置MSVC/SDK，使用工程stable；没有改全局环境。最终用户要求继续D盘，采用本树 `target`、进程级 `CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`、`CARGO_INCREMENTAL=0`。C盘阶段只因当时D盘os112临时安排，既有原始证据保留，未冒充D盘执行。

| 执行 | 时段（UTC+8，日志创建/最后写入） | 结果/证据 |
| --- | --- | --- |
| 首次定向编译 | 早期在途 | E0282类型推断失败，显式Result类型后修复；仅当次工具输出，未独立保存原始日志 |
| 首轮 `cargo test --locked --test git_diff -- --test-threads=1` | 早期D target | 101，9过5失败；[test-01](证据/test-01.log)。4项为Git check-ignore不支持继承literal pathspec环境，另1为merge夹具用户身份缺失而未真冲突。分别修子命令环境及有效夹具，保留原失败 |
| 第二轮定向编译 | D盘耗尽时 | 编译失败、测试用例未执行；[test-02](证据/test-02.log)实际含Compiling、rustc-LLVM no space与fingerprint创建失败。外层工具退出1并报启动/日志错误，cargo单独退出码未捕获；早期据工具错误误称未启动/空日志，现按原日志纠正。经理保全/压缩本轮target；本人未删除旧树 |
| C target中间定向及最终尝试 | 01:48:18～01:50:01 | 两次15/15，[test-03](证据/test-03.log)、[test-final](证据/test-final.log)；Clippy唯一collapsible_if失败，[clippy](证据/clippy.log)。中断后确认后台已完成，修该lint；C阶段fmt空输出但未捕获外层退出码，不作为最终门禁 |
| `. ./build.ps1 -Action check` | 01:57:59～01:58:37 | 0，[check-d-final](证据/check-d-final.log)；lib导出未合，编译模块本体的证据在下列path集成测试/Clippy |
| `cargo test --locked --test git_diff -- --test-threads=1` | 01:58:37～01:59:20 | 0，15过0失败0忽略、测试22.63s，[test-d-final](证据/test-d-final.log)；对应a5c2dd1 |
| `cargo clippy --locked --test git_diff -- -D warnings` | 01:59:20～01:59:30 | 0，[clippy-d-final](证据/clippy-d-final.log) |
| `cargo fmt --all -- --check` | 01:59:30 | 0，[fmt-d-final](证据/fmt-d-final.log)；暂存diff--check也0 |
| 最终补强定向 `cargo test --locked --test git_diff absent_promisor_object_is_unavailable_without_network_or_writes -- --exact` | 02:03:04～02:03:07 | 0，1过/14过滤，[test-offline](证据/test-offline.log)；对应0def34f，fmt0 |
| 补强后额外Clippy首次 | 02:03:24 | 101，独立新进程未初始化MSVC，找不到cl.exe；环境失败保留于[clippy-offline](证据/clippy-offline.log) |
| 重新dot-source check后补强Clippy | 02:03:38～02:03:46 | check0、Clippy0，[check-offline-02](证据/check-offline-02.log)、[clippy-offline-02](证据/clippy-offline-02.log) |
| 仅测试helper可见性修订后的check/Clippy/fmt | 02:09:43～02:09:49 | 全0，[check-helper](证据/check-helper.log)、[clippy-helper](证据/clippy-helper.log)、[fmt-helper](证据/fmt-helper.log)；最终1b80486未复跑矩阵，组合all-targets仍由C执行 |

日志原字节存独立树 `../nhc-d-git-diff/.local/d-r4-evidence`；发布副本仅将机器路径替换为仓库相对逻辑位置、统一LF/UTF-8无BOM，双hash在manifest。时间字段为日志文件创建及最后写入事实，不冒充精确进程开始/退出计时。上游proc-macro-error2 future-incompat提示保留，不是本轮新增依赖。

## 交付状态与下一步

最终候选干净冻结，经理可审后串行合入B；本人不合他人分支、不操作共享index、不推送。Engine gate/Task错误/生产lib导出由B，HTTP/CLI及最终组合五门禁由C。未运行全workspace test、WASM检查、release、付费live或产品Host端到端；没有借组件测试声称NEXT-06整体验收。

下一条操作：保持源码冻结，等经理明确review；若需要修订，沿本任务/执行ID新增提交及证据，保留既有失败和候选来源。
