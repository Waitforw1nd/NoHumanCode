# NEXT-18 D attempt2 审查与 D18-R08

2026-10-31，项目经理。原 D-R16 接续；基线、文件所有权及整合责任沿用 NEXT-18 契约。主线仍为 NEXT-17，本文不是 NEXT-18 联合验收。

## 原件与结果

受测候选 `896b690938cb31946acb2c4030de76f58eb13d43`，NoManCode `f687e5f5babcded3585cf56fabc216976434c4f0`，Rust `5e1282deb5df607959c620c25ce440d4cf26e7fd`。

check/build exit0；lib 314 passed / 0 failed / 0 ignored，原 D18-R07 已动态闭合。schema integration exit101、6 passed / 1 failed / 0 ignored；七历史集成、Clippy、fmt 未执行。不能以库通过替代组件完成。

经理独立核对20份文本原件/发布副本的长度、SHA256及仅路径替换；8份源码清单一致，134项与受测Git blob一致。13个崩溃现场共234文件的hash/长度、阶段/PID/wait/signal、两次不同进程恢复通过输出均相符。只读 immutable 查询独立 queryable 快照，九迁移阶段的版本/marker/旧rowid/负时间戳、四管理意图阶段的desired/receipt均符合提交前后语义。没有用SQLite打开或恢复原始DB/WAL/SHM，没有运行产品测试。

核验见[经理原件核验](../../../项目经理/审查记录/2026-10-31NEXT-18-D组件attempt2原件核验.json)。原manifest SHA256 `89E7414E0A8CB184CCBE78ECE32C8026502DD2E4D5053E9D86CBDC010A8C67B9`；原失败与原现场保留。

## D18-R08：损坏夹具未能构造目标状态

`tests/runtime_management_schema.rs:325` 在注入缺shell行的SQL时收到SQLite 787外键错误，尚未进入 `Store::open`。旧shell receipt仍引用该行；失败不能证明Host拒绝或迁移行为。

纠正先前解释：historical16内部连接已经结束，注入使用新连接，不能声称外键设置由前一连接继承。新候选读取并断言注入连接的实际PRAGMA，然后仅为这个故意损坏的case临时关闭外键，删除目标行后恢复开启。进入Host前断言目标行缺失、rowid72的old-shell receipt仍在、foreign_key_check准确报告该孤儿。

经理进一步核对实际调用链：Store::open先调用repository::verify_schema_8，后者执行全库foreign_key_check。真实shell孤儿会先以“schema 8 外键检查失败”拒绝，因此原来要求此case命中“plugin desired state invalid”不符合代码。接受只将该case的精确reason改为schema8拒绝；这覆盖目标损坏的Host fail-closed，不宣称命中了runtime专用校验分支。其余case、合法control、schema16未推进及其他精确reason均保留。未修改生产校验或删除receipt回避约束。

固定修订 `283a29858b914c1f26318f3a9603739ebc46be1d`，NoManCode `88d9baeda5df6996b3b3b48957aaea60bcd523c7`，Rust `12451bb973c48cf2bbbeade0c89fd5a776c905e6`。唯一差异为上述schema集成文件，经理已读完整diff并核实clean；文件SHA256 `00C873B846E523EDB4833BC6E052F229884C0DC45240079E98D30CE86A0263DD`。静态修订可进入定向验证，动态闭合待结果。

## 定向 attempt3 窗口

原D独占窗口；B/C保持source/index/no-build冻结。本人核收固定三SHA、clean、无重型进程后顺序执行，首失败停止：

1. `cargo check --offline --locked --all-targets`
2. `cargo build --offline --locked --bin peachsh`
3. `cargo test --offline --locked --test runtime_management_schema -- --nocapture`
4. `cargo test --offline --locked --test approval_gate --test checkpoint --test project_migration --test workspace_changes --test host_configuration --test host_configuration_schema --test project_context -- --nocapture`
5. `cargo clippy --offline --locked --all-targets -- -D warnings`
6. `cargo fmt --all -- --check`

原E树target缓存可复用，TMP/TEMP/log独立；offline/locked、debug0/incremental0/jobs2及已有工具链只读不变，旧fixture的rust-app/target前置保持。不更改timeout、测试并发或断言。每步记录命令、退出、时间、起止清单及实际CLI前置。

补充前置裁决：D开跑前发现缓存debug/peachsh.exe从attempt2 build后的 `1290A953F72F1AD9BCE6F1FBA7BDEDEBA174E290BA51576D5D3D06D296B7CEF1` 变为 `7E5B7AE93F78D37F71DDB5D2DAAE95367F85877B25957DCC1741B745DED44625`。EXE修改时间06:39:05.3059640Z、bin fingerprint修改时间06:39:05.3687445Z处于上一轮schema构建06:38:49.8727046Z至06:39:06.0145650Z内，日志确有编译；但postflight未记录CLI事后hash，时间相关性不冒充完整归因或字节相同证据。因此本轮显式从固定源码build并记录真实新hash，不人为恢复旧产物。检查时未启动构建测试。

本轮仅integration夹具改变，lib/生产/build输入未改；不重复已有效的lib/crash运行。314/0/0仍准确归属896b690，其适用范围由单文件差异证明；不能将它写成283a298执行结果。最终C必须对固定完整组合执行全部五门禁。D组件尚未验收，后续仍需原件审查、C公开组件和最终门禁、经理M01–M16联合验收及main整合。
