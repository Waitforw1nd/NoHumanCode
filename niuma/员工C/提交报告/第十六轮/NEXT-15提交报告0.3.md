# NEXT-15 员工C报告0.3：真实组合组件验证与冻结交接

日期：2026-09-28；员工C / GPT-6-astra；同执行ID C-R16-01-20260928-E01。接续原正式任务及经理在当前员工C对话的完整B/D依赖整合、组件解冻指令；没有新任务、新代理或主动跨对话消息。

**状态：限定bin check和I01/I07/I08/I10三个专项文件已实际通过，11 passed / 0 failed / 0 ignored；首轮I10失败完整保留。交付候选source/index clean/no-build持续冻结，等待经理审查与整合。最终五门禁尚未授权、未执行；不自行APPROVED，不宣称完整第一阶段完成。**

## 1. 接收、受测与交付候选

本人源码树为仓库根相对 ../nhc-c-host-setup-cli，分支codex/c/host-setup-cli。源提交由C在本人树执行；主仓index、人员档案提交与跨树merge仅经理，本人未操作。主仓后续管理HEAD不用于对齐、reset或自行merge。

| 节点 | HEAD | NoManCode tree | Rust tree |
|---|---|---|---|
| 原正式开发基线 | 5194099ee4d1eb72af5f286826c72c6fd19d29e8 | f48df8d01f64095ad32b1412ac5a4db45b849182 | dfdedbfda40aba9502f1606cc466f2f32aa4e3d0 |
| 本次完整B/D接收与attempt1 | 8ed3165099a53139255d415ba97242f818cbc4b0 | 31c19c0e36049721d2ee024127994b2142dc83d2 | c488adeee35b08dbffd92558e3082dd3d54685bd |
| C最小修复、attempt2受测 | aac520b28eed4f3a60cb9e9d2492bd231b13f96e | bcf13e4e0c50179f8c041773eed97f0e01758889 | 63a2b26952faa94b8b60f3bcb0eb92c92a730532 |
| 当前交付：仅再更新CLI验收说明 | d43db398244850d69de96c3e3c616b428f872d39 | 02cb4fb0533639970fd27bba93fea50a43895c43 | 63a2b26952faa94b8b60f3bcb0eb92c92a730532 |

2026-09-28T15:02:54.8667151Z本人实核8ed3165完整组合，真实B host_configuration/fresh_settings/Engine及D credential_input导出存在，status/index空。D先前合后接收事实见保留的报告0.2；本轮不重写它。

当前交付HEAD比受测HEAD仅多NoManCode/docs/cli-first-release-acceptance.md说明更新。两个HEAD的Rust tree完全相同，git diff --exit-code aac520b28eed4f3a60cb9e9d2492bd231b13f96e HEAD -- NoManCode/rust-app实际exit0。没有把文档提交写成又重跑一轮；所有动态结果明确绑定aac520b。

## 2. 本次修复与文件所有权

本次相对8ed3165只修改两个C授权文件：

- NoManCode/rust-app/tests/host_initialization_workflow.rs：network authorize现有真实CLI输出NetworkDecision，授权对象在authorization内。首轮测试误读外层purposes得到Null；修正为decision.authorization，并增加decision.replayed=false断言。用途仍严格为completion，后续authorization_id/binding_digest和重启断言保持，未弱化断言、未改产品API。
- NoManCode/docs/cli-first-release-acceptance.md：记录真实组件结果与受测SHA/Rust tree，替换已经过时的“依赖未合/专项未执行”，明确最终五门禁、完整主路径和阶段一仍待验收。

首候选七文件实现见报告0.1。此次没有改B/D文件、公共DTO、Engine、Store、迁移、lib、Cargo或其它测试；没有stub、Store预注入路由或直接写SQLite初始化。现无须交B/D修复的组件阻塞。

## 3. 原命令、UTC与结果

所有shell显式E活动目录。下列cargo命令的cwd均为本人树NoManCode/rust-app；驱动从本人树根执行，内部从自身目录定位。限定编译不是最终门禁check。

    pwsh -NoProfile -File .local/next15-component.ps1 -Attempt attempt1
    pwsh -NoProfile -File .local/next15-component.ps1 -Attempt attempt2

| 串行组件原命令 | attempt1（8ed3165） | attempt2（aac520b） |
|---|---|---|
| cargo check -p peachsh --bin peachsh --locked --offline | 15:05:24.9789846Z–15:06:10.2307237Z；exit0 | 15:11:02.1444339Z–15:11:02.5045290Z；exit0 |
| cargo test --locked --offline --test host_configuration_http -- --nocapture | 15:06:10.5117522Z–15:07:01.1050541Z；3/0/0；exit0 | 15:11:02.7804786Z–15:11:03.2864328Z；3/0/0；exit0 |
| cargo test --locked --offline --test host_configuration_cli -- --nocapture | 15:07:01.3645669Z–15:07:04.6300454Z；6/0/0；exit0 | 15:11:03.5725562Z–15:11:05.6601586Z；6/0/0；exit0 |
| cargo test --locked --offline --test host_initialization_workflow -- --nocapture | 15:07:04.8554287Z–15:07:10.2719997Z；1/1/0；exit101，停止 | 15:11:05.8995327Z–15:11:11.4960172Z；2/0/0；exit0 |

以上日期均为2026-09-28，时间均UTC；三元统计为passed/failed/ignored。attempt1外层pwsh工具进程exit1与其cargo exit101分别记载；attempt2外层exit0。第二轮11/0/0不累计第一轮重复通过数，没有ignored或过滤测试。

修复后在本人Rust cwd实际执行以下单文件限定格式命令，各exit0；git diff --check亦0，随后提交aac520b成功exit0。这里不是最终五门禁fmt：

    rustfmt --edition 2024 --config style_edition=2024,skip_children=true tests/host_initialization_workflow.rs
    rustfmt --check --edition 2024 --config style_edition=2024,skip_children=true tests/host_initialization_workflow.rs
    git diff --check
    git add -- tests/host_initialization_workflow.rs
    git commit -m 'test(host): assert nested network authorization in initialization flow'

文档同步后，仅暂存该文档并提交，命令及结果：

    git diff --check
    git add -- NoManCode/docs/cli-first-release-acceptance.md
    git commit -m 'docs(cli): record verified secure initialization component results'
    git diff --exit-code aac520b28eed4f3a60cb9e9d2492bd231b13f96e HEAD -- NoManCode/rust-app

上述各步均0；暂存前实核index空且修改范围准确。格式/提交原日志及冻结回执与完整meta一并发布。

## 4. 已执行事实与边界

| ID | 实际组件结果及证据 |
|---|---|
| I01 | 真实binary空库安全默认、显式legacy导入与已有库行为分离；邻近合成旧设置及环境凭据不会被默认空库读取；check初始化写入/help语义均测试。第二轮1项通过。 |
| I07 | 3个真实HTTP/Engine专项通过：三入口Host/Origin/site/token、严格嵌套JSON/类型/重复及未知字段/唯一key/限制/不安全目标拒绝、原子拒绝、原回执回放/当前snapshot/固定错误。 |
| I08 | 6个真实CLI专项通过：输入/参数失败无PUT及无输入回显，安全输出，原回执lookup与404在途未知边界，可信错误与不可信错误区分，真实TCP收到完整PUT后断线/截断/重定向/超大体/恶意成功响应及stdout断管均unknown且单次PUT，不自动重发或换key。 |
| I10 | 同一初始不存在DB、真实Host和CLI配置路由→Project权限/选择→预算→preview/Completion授权→一次本地mock Completion→实际usage核验→更新显示名→kill+wait→同DB重启核查，第二轮通过。 |
| I11 | 最终固定组合check→test→fmt→clippy→wasm-check未放行、未执行。完整组合回归和D真Host crash不由本报告替代。 |

I10关键实际断言与日志：配置完成时Provider=0且授权/network call/budget call表计数为0；显式Project权限、选择、预算、Completion授权完成时Provider仍0。真实mock收到配置的测试凭据及模型，只请求一次Completion，Models=0。Task completed，tools=false、allow_commands=false；network call为completion，budget实际usage=5、spent_tokens=5、held_tokens=0。

显示名修改后原operation receipt revision=1保持，当前GET revision=2；预算不变。真实Host先kill并wait退出，再对同一数据目录启动，bootstrap token改变；原receipt、当前配置、Project选择、预算、授权active与binding_digest及Run已完成状态仍在，Provider仍1、无重放。成功日志中I10 PID为47388→5084，均有kill+wait；I01另有其独立启动实例，不能把两项并行测试的PID混为同一DB。

I10只覆盖本片最小初始化链，不代表默认插件组合逐项装配/撤销，或文件读搜/写入/Shell审批/Diff/Checkpoint/恢复/新Turn的完整第一阶段验收。不存在付费Provider访问或release。

## 5. 失败、工具问题与未执行

首轮workflow.log保留原tests/host_initialization_workflow.rs:460的Null与[completion]断言失败。该时点在network authorize之后、Completion之前；不能将首轮记为Completion/重启成功。修复后同一组件驱动从check重起，生成新attempt2目录，不覆盖attempt1。

工具层记录区别于测试结果：一次工作目录转义问题导致Windows error267、未创建进程；修正后才执行attempt1。一次只读rg正则失败和不存在的network_workflow.rs查询随后已定位到cli.rs；一次functions.exec脚本引号语法错误发生在patch/shell之前，重试才实际写入。相关摘要为本人转录说明，不伪装Cargo原日志。

冻结阶段使用无正则的精确进程名筛选再次核本树cargo/rustc/rustfmt/clippy-driver/Host/tests为空。freeze-verification.json与orchestration-notes.json中对首次筛选写了“overescaped”的文字判断，该根因未单独证明；此处澄清，不将疑虑当已证实产品或测试失败。最终冻结以再次实核的状态为准。末尾探查并不存在的next15-receive-b目录导致该工具调用外层exit1；此前生成的freeze-verification.json成功保留，之后已读回核对，没有冒充该整条shell exit0。

旧0.1/0.2及其早期格式/编排失败原件保持。未执行本轮全workspace测试、最终fmt/clippy/wasm-check、release、paid live、push；不把NEXT-14旧结果或其他员工自报当本轮C通过。

## 6. 环境与原件/发布证据

只读使用已有rustc1.98.1 (48a229cea 2026-09-01)、cargo1.98.1 (797e8a9bc 2026-08-05)、Rust目录rustfmt1.9.0、MSVC14.43.34808和SDK10.0.22621.0。未安装或更新。驱动只采用现有构建脚本的环境定位逻辑，不进入release动作。

TMP/TEMP为本人树.local/tmp，CARGO_TARGET_DIR为本人树.local/target；现有fixture所需rust-app/target也在本人E树。设置RUSTUP_AUTO_INSTALL=0、CARGO_NET_OFFLINE=true、CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_INCREMENTAL=0、CARGO_BUILD_JOBS=2。所有工程/临时文件/target/日志仅E；没有读真实密钥，合成秘密不在报告展示。

原件位于仓库根相对 ../nhc-c-host-setup-cli/.local/logs/ 下的next15-components-attempt1、next15-components-attempt2、next15-i10-fix、next15-component-freeze；另含.local/next15-component.ps1驱动。发布清单：[NEXT-15证据/0.3/manifest.json](NEXT-15证据/0.3/manifest.json)。共24对证据，15对逐字节相同、9份仅发布副本相对化工作目录/工具链路径；原件不动，清单逐件记录原/发布SHA256、字节数和改写逻辑别名。不是声称路径改变后的hash仍等于原件。清单同时绑定七个C授权文件的SHA256与Git blob。

manifest SHA256：86B28CACA153F68DFC8FFF0790AF96365C4873B5475FEB5DBF634B9858E6EAA0。

## 7. 当前明确冻结与下一第一操作

2026-09-28T15:12:51.7132852Z形成d43db398244850d69de96c3e3c616b428f872d39冻结回执，15:13:30.2104610Z再次实核HEAD/两tree/分支、status/index空、本树无构建或Host/测试进程。**本人从该节点持续source/index clean/no-build冻结，允许经理按既定职责审查和串行跨树整合；本人不自行merge或推进源码。** 人员报告/身份收尾不解除源码冻结。

下一第一操作：经理整合后本人先实核完整新HEAD/两tree/status/index，按明确续办指令继续；最终五门禁只有经理固定完整组合并放行后由C唯一串行执行check→test→fmt→clippy→wasm-check，任一失败停止，源码变化从check重起。本报告不是放行、APPROVED、合main或发布证明。主仓本人档案由经理读取与提交，本人不操作主仓index，不主动跨对话发送消息。
归档复核补记：24对原/发布SHA256与七文件源码绑定均再次相符。发布路径初次正则误将注册表HKLM的末尾M识别为盘符，校验工具exit1；逐行确认是三个注册表发现入口，未改这些合法脚本行。改用实际盘符路径检查后0命中，未改原件或manifest。0.3在该校验完成后封存。随后经理在当前对话明确续办旧HTML/JS与settings HTTP级动态兼容补证，另开报告0.4；本报告冻结描述为当时交接事实，新授权与解冻见本人最新身份，不能用于随后的持续冻结推断。