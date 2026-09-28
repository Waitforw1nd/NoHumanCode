# NEXT-15 员工B提交报告0.1：真实公共API检查点

2026-09-28；员工B / GPT-6-astra；执行 B-R14-01-20260928-E01，同任务手动接续。状态：组件检查点，提交待审，未自行APPROVED。用户本对话是正式派发，未重新建立任务或subagent。

## 基线与候选

- 基线完整HEAD：5194099ee4d1eb72af5f286826c72c6fd19d29e8；NoManCode f48df8d01f64095ad32b1412ac5a4db45b849182；Rust dfdedbfda40aba9502f1606cc466f2f32aa4e3d0。初始工作区/index/unmerged为空，无继承修改。
- 本检查点：7cddb4282382cc692b782d5e3c19a250b23a25ba；NoManCode f3507a1411b207de4ff7352b1ad8559c9672750d；Rust fa004df2091cd3f217c8c8ab29ee15c610cfbf4b。分支 codex/b/host-route-config；源码树 ../nhc-b-host-route-config。提交后status/index空，当前继续独立组件，**不是持续source/no-build冻结回执**。
- 八个文件：src/host_configuration.rs、engine.rs、store.rs、repository.rs、network_authorization.rs、lib.rs；tests/host_configuration.rs、host_configuration_schema.rs，均相对NoManCode/rust-app。未改C/D文件，无主仓index或跨树merge。

## 真实API与实现

Engine提供契约三个真实方法：host_config、host_config_operation、async configure_host_route；Store对应方法同样验证输入。HostConfigError枚举提供code/message/status/retryable，纯固定安全文案。host_configuration导出所有冻结DTO；CredentialUpdate无Debug/Serialize，自定义严格Deserialize拒绝keep携value、重复字段及未知字段。

schema16 marker为host-route-configuration-v1；host_config_state单例、host_config_requests受约束DDL及持久行交叉校验。请求采用v1长度编码SHA256，仅DPAPI封装摘要入blob；配置、secret、revision、原有网络Subject/grant刷新和receipt在同一Immediate事务。回放优先于busy/CAS，no-op写receipt不增revision，原回执查询不依赖新输入秘密。

fresh_settings仅原子建立空routes/newapi、concurrency3、revision0，已有settings返回且不覆盖；有历史但丢settings则拒绝。save_settings、put_secret、旧save_network_configuration统一维护Host revision。未增加授权/联网/Project选择/预算变更。D真实输入模块尚未整合，lib不导出不存在模块。

## 实际组件结果

原命令、UTC、工具版本、源码SHA256、退出码见[双hash清单](证据0.1/manifest.json)和各metadata。cwd为本人树NoManCode/rust-app，CARGO_TARGET_DIR/TMP/TEMP均.local/next15；debug0/incremental0/jobs2；已安装MSVC只读，不安装更新。

| 证据 | 命令/结果 |
|---|---|
| compile-01 | 环境exit1：动态脚本PSScriptRoot为空；Cargo未运行，保留原runner。 |
| compile-02 | 环境exit1：缺MSVC根进程变量；Cargo未运行。 |
| compile-03 | cargo check --lib --offline --locked；exit0，有一处括号warning，后续已修。 |
| host-tests-01 | cargo test --offline --locked --test host_configuration --test host_configuration_schema -- --nocapture；exit101，测试字符串replace参数编译错误，测试未执行。 |
| host-tests-02 | 同命令exit101；配置11通过/1失败：Serde unit variant漏拒绝keep携value；schema组未执行。 |
| host-tests-03 | 同命令exit0；配置12通过、schema6通过，合计18/0/0；针对与本候选相同源码内容。 |

I02受控schema15非空夹具旧行/rowid/blob原样，双open、迁移失败原子、错DDL/marker/receipt拒绝；并非旧二进制fixture。I03两Store同key与CAS竞争、异secret/ID/expected/mode冲突、两个no-op、旧低层revision与溢出回滚。I04地址/凭据边界、env清除、legacy blob保留、不可解密fail closed；I06插receipt触发器失败后全表逐值相等。

## 未完成与下一步

I05完整双Project/旧Task/预算unknown矩阵、真实Engine活跃网络slot、旧账户/设置生产入口与历史B fixture补证仍进行；I09仅cfg(test)事务前后停点，真实kill+wait由D负责。I07/I08/I10由C/D联测，I11最终五门禁未放行、未执行。不把18个专项当完整验收。

下一操作：继续B-owned组件及原始证据，完成后新增报告0.2并给固定SHA和当前clean/no-build冻结。需要D真实模块时由经理串行整合，再由B实核接收后唯一添加lib导出。报告只交用户转交经理，不自动跨对话发消息。无release/live/push/reset/clean/stash/旧树清理。