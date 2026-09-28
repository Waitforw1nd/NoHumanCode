# NEXT-15 员工B提交报告0.2：组件候选与真实输入导出检查点

日期：2026-09-28；更新人：员工B / GPT-6-astra；执行ID B-R14-01-20260928-E01，同任务手动接续。状态：提交待审；不是APPROVED或NEXT-15整体完成。0.1与原失败全部保留。

## 当前可整合回执

源码树 ../nhc-b-host-route-config，分支 codex/b/host-route-config。经理已普通merge C组合（含已审D输入）；本人实核接收 a8dd7f6fcf77b5a9f43791a64470f287f0a3a69f / NoManCode a0b9c1b9c69df6cf6a45bbeace8c7259fab2cb99 / Rust d5b83e187319fc671e3853b1ebb51702a32ff9a8，status/index/unmerged均空，真实credential_input.rs存在，D真Host崩溃模块尚不存在。

按经理明确单行窗口，B只在lib.rs加入实际 pub mod credential_input，提交 c94a48daded1b0098b560e6f8761d64a7e8c1096；NoManCode tree 31c19c0e36049721d2ee024127994b2142dc83d2；Rust tree c488adeee35b08dbffd92558e3082dd3d54685bd。git diff --check、提交及合后查询均退出0。2026-09-28T15:00:16.5628067Z再次实核source/index/unmerged空，无cargo/rustc/clippy/rustfmt/link及相关测试进程，见[冻结原命令记录](证据0.2/freeze-export-02.json)。

**B持续source/index/no-build冻结供经理交C执行真实接口组件，等待经理明确释放后才继续补充03夹具。** 报告/证据整理不等于解除源码冻结。此导出检查点未编译、未动态测试；下文118项只属于旧固定f340358候选，不能外推为本新组合通过。D崩溃模块未合入，不导出不存在模块，不造stub。

## 基线、提交与唯一写权

- 原完整基线5194099ee4d1eb72af5f286826c72c6fd19d29e8；NoManCode f48df8d01f64095ad32b1412ac5a4db45b849182；Rust dfdedbfda40aba9502f1606cc466f2f32aa4e3d0。
- API检查点7cddb4282382cc692b782d5e3c19a250b23a25ba；NoManCode f3507a1411b207de4ff7352b1ad8559c9672750d；Rust fa004df2091cd3f217c8c8ab29ee15c610cfbf4b，详[0.1](NEXT-15提交报告0.1.md)。
- 扩展组件提交f27cd04d2d289749256aeb0abc81187e74003d4e；NoManCode 965c922edef4660b3eddce35aec8a318ba665ba8；Rust f701d1d53480bd73b050d86367505bf298a0febf。其网络兼容测试1项失败，已由下一提交修正，不称该版本全过。
- **实际通过候选f340358a80cb6a8d8fd74acf313bee02386e3d99**；NoManCode cb6d30758e63f2d245d1ca3fa2595fa77f74e58a；Rust c613c0b68604ccb6d69a007a7e8ff9c371536721。两批测试的原metadata均记录此HEAD、两tree和空status/index。
- B至f340358只改NoManCode/rust-app下八文件：src/host_configuration.rs、engine.rs、store.rs、repository.rs、network_authorization.rs、lib.rs；tests/host_configuration.rs、host_configuration_schema.rs。之后B唯一新增改动是c94a48d的lib单行导出；C/D内容由经理合入，贡献不归B。

所有跨树merge、主仓index及档案提交均由经理；本人只在授权独立树提交源码，人员记录只写活动主仓niuma/员工B，未写本人工作树的旧档案副本。

## 已完成实现与关键兼容修复

1. host_configuration冻结DTO、安全HostConfigError code/message/status/retryable，以及Engine的host_config、host_config_operation、async configure_host_route均为真实API，Store对应入口共享验证。CredentialUpdate无Debug/Serialize，自定义严格Deserialize拒绝keep携value、重复字段和未知字段；地址、路由ID、凭据长度等边界由同一验证入口处理。
2. schema16新增host_config_state单例revision与host_config_requests持久回执，marker为host-route-configuration-v1。迁移、DDL/约束/marker/回执列一致性及密文摘要有效性严格校验，不以降低版本号替代真实旧schema夹具。
3. 单个Immediate事务内完成单路由、DPAPI凭据、revision、网络绑定刷新及receipt。持久回放优先于busy/CAS；no-op持久化回执但不增加revision。请求采用带版本与长度前缀的SHA256指纹，仅DPAPI保护后的摘要入库；拒绝明文秘密或裸摘要持久化。
4. fresh_settings只建立空routes/newapi、concurrency3、revision0，不读legacy文件或环境变量；已有settings不覆盖，有历史而丢settings时拒绝冒充fresh。配置不自动授权、不联网、不创建或换绑Project、不清除预算unknown。
5. 旧save_settings、put_secret和生产save_network_configuration统一覆盖Host revision。真实旧测试发现低层写入若立即刷新网络绑定会把原预期Stale变成Required；最终通过save_configuration_tx的refresh_network参数保持raw save_settings/put_secret原延迟绑定检查语义，仍原子增加revision。新配置与旧生产保存走刷新路径；生产no-op仍复核环境驱动的绑定变化，但不增加Host revision。
6. Engine/Repository原授权范围内的历史fixture精确拆除schema16 sidecar、迁移后只显式加入新单例/空表/marker，保留原逐行、rowid、blob和业务断言。

## 固定f340358真实组件证据

工作目录为本人源码树NoManCode/rust-app。原始Cargo命令保持如下；所有target/TMP/TEMP及日志都在本人E树.local/next15，debug0、incremental0、jobs2。仅使用已安装工具链，MSVC/SDK只读，无安装更新。发布副本中的工具链路径为约定逻辑别名，不是可直接运行目录。

    cargo test --offline --locked --test host_configuration --test host_configuration_schema --test network_authorization --test budget -- --nocapture
    cargo test --offline --locked --lib -- host_configuration:: repository:: store:: engine:: --nocapture

| 原始证据标签 | 实际UTC时间 | 退出码与原始结果 |
| --- | --- | --- |
| candidate-integration-02 | 2026-09-28T14:48:20.7599518Z至14:48:26.2844543Z | exit0；配置18、schema6、网络授权16、预算3，共43通过，0失败/0忽略 |
| candidate-unit-01 | 2026-09-28T14:49:01.7566598Z至14:49:25.4916797Z | exit0；Host事务1、Repository24、Store11、Engine39，共75通过，0失败/0忽略，149 filtered |

合计118个不同组件用例通过，不把历史重复运行再累加，也不是最终五门禁。Host transaction_child在无测试环境变量时执行实际Store事务smoke，本次通过不代表杀进程或HTTP覆盖。

主要动态覆盖：

- I02：受控非空schema15夹具带真实Task/网络与预算unknown数据，连续两次open保留旧行/rowid/opaque blob；并发open、迁移失败原子回滚、坏DDL/marker/singleton/receipt拒绝。它不是历史二进制生成的fixture。
- I03/I04：同key/不同key双Store竞争、same key异secret唯一胜者、两个no-op；不同route/expected/mode/secret回放冲突；revision overflow、不可解密凭据、env清除、legacy命名空间/密文保留、裸摘要不存在与严格输入边界。
- I05/I06：真实Engine活跃Task及独立in-flight route-check阻止新写，原成功请求仍可回放；双Project既有grant身份保留，绑定变化时stale、metadata-only不误伤；旧完成Task与权限/绑定快照保持。真实route-check中止再recover形成unknown，hold100/spent5及另一Project预算保持。receipt插入失败后全表逐值回滚。
- 实际配置操作不会产生Provider请求、Project注册、授权或预算清理。旧账户生产保存、metadata变动、环境变动下no-op复核均有专项；旧网络与预算集亦在f340358重跑通过。

## 历史失败与逐轮修复

0.1中的两次环境失败、测试编译失败和keep严格解析失败继续保留。以下11组原log/metadata新增发布，不覆盖0.1。

| 标签 | 原命令或类别 | 原结果与处理 |
| --- | --- | --- |
| host-matrix-01 | cargo test --offline --locked --test host_configuration -- --nocapture | exit101，测试误用不存在的unknown_count；改用真实unresolved_count及SQL unknown断言，测试未执行 |
| host-matrix-02 | 同上 | exit0，14/0/0 |
| host-matrix-03 | cargo test --offline --locked --test host_configuration --test host_configuration_schema -- --nocapture | exit0，16+6通过 |
| repository-01 | cargo test --offline --locked --lib repository:: -- --nocapture | exit0，24/0/0 |
| engine-01 | cargo test --offline --locked --lib engine:: -- --nocapture | exit101，37通过/2失败；Engine快照SELECT *与Repository SELECT rowid,*差异导致新单例预期多一列，最小夹具修正，业务断言保留 |
| engine-02 | 同上 | exit0，39/0/0 |
| store-01 | cargo test --offline --locked --lib store:: -- --nocapture | exit0，11/0/0 |
| candidate-integration-01 | 同固定候选integration命令 | f27cd04d上exit101；预算3+配置18+schema6通过，网络15通过/1失败，raw写入Required/Stale回归如前述 |
| compatibility-fix-01 | cargo test --offline --locked --test network_authorization --test host_configuration -- --nocapture | 在途修复exit0，配置18+网络16；随后固定f340358并重跑两批 |
| candidate-integration-02 | 见固定候选命令 | f340358干净候选exit0，43通过 |
| candidate-unit-01 | 见固定候选命令 | f340358干净候选exit0，75通过 |

host-matrix/repository/engine/store早期在7cddb HEAD带在途修改，compatibility-fix-01在f27cd04d HEAD带在途修改；metadata如实保留status及src哈希，不把HEAD两tree当作那次完整受测内容。早期runner只采src顶层文件哈希，没有逐个记录integration test在途哈希；本报告不据此宣称早期run针对最终tree。最终两批均以f340358全树已提交且status/index空重跑，消除本候选归属歧义。

本次整理过程中两次脚本在JavaScript解析阶段SyntaxError（路径转义、换行表达式），对应shell均未调用、未改证据；改用raw文本和字符编码表达式后执行成功。另一次只读核收末尾猜错补充03文件名使组合shell返回exit1，前面的Git/模块读取成功；随后用rg定位并完整读取真实补充03。这些不是产品测试失败，不冒充exit0核收命令；最终独立冻结记录中的八条Git命令各自exit0。 最后档案git diff --check首次exit1是本轮身份新增段落CRLF被识别为尾空白；仅规范该新增前缀为LF，历史正文及原证据不改，重查exit0。

## 证据发布与hash

[证据0.2清单](证据0.2/manifest.json)新增24组原始/发布映射：11组log+metadata（22文件）、实际runner、最新导出冻结记录。每项包含原始和发布SHA256及字节数；已重新从磁盘逐项核对24/24一致。仅发布副本作路径相对化/UTF-8规范化，原始文件未改写。清单SHA256为98EC881D543B80E68A3AEF134EAC8F87A3BABFC5599E497A4000CB50910DB12F。

原件位于 ../nhc-b-host-route-config/NoManCode/rust-app/.local/next15/<label>/，发布文件见本目录证据0.2。命令、UTC、退出码、实际受测HEAD/tree/status/index、工具版本均见对应metadata。路径扫描剩余命中是本地HTTP测试URL的尾部字母加冒号，不是机器绝对文件路径。0.1原13份证据和清单保留原样。

## 尚未执行、所有权与下一步

- **新组合c94a48d未编译/测试，B限定Clippy未执行；最终五门禁未放行、未执行。** C只在经理明确放行后唯一串行执行最终五门禁。无release、付费live、push或安装更新。
- 补充02已完整接收：I09由D唯一src/host_configuration_crash_tests.rs通过真实Host/Engine/C路由与认证HTTP执行kill+wait、两次重开；不得用B当前Store smoke冒充。B的cfg(test)停点before_commit/after_commit、NHC_HOST_CONFIG_TEST_ROOT/PHASE及ready PID文件已提供，真实D文件合入后才由B唯一lib注册。
- 补充03已完整接收：tests/approval_gate.rs、checkpoint.rs、project_context.rs、project_migration.rs、workspace_changes.rs五文件，仅历史迁移夹具/精确schema16期望归B；当前等待经理释放窗口，尚未开始。不改业务断言、不删测试、不整仓替换版本数字。
- 另有静态待定位事项不在补充03授权范围：src/plugin_management.rs约412/461/714行，以及tests/support/project_fixture.rs的drop_project_schema约47行（network_fixture委托它）。这些是静态疑点，未运行失败证据；需经理登记写权或确认由谁修，不自行修改。
- I07/I08/I10完整C/D联测、I09真实Host崩溃证据、经理review与固定联合候选均未完成。118组件结果不能替代完整CLI/HTTP或新用户端到端验收。

下一第一操作：经理先取c94a48d导出检查点交C；B保持source/index/no-build冻结，收到明确释放后按补充03继续五个精确夹具与原B组件，形成下一固定候选和新增报告。所有报告在当前对话及本人主仓档案交付，经理直接读取；B不自动跨对话发送、不自行整合或操作主仓index。

