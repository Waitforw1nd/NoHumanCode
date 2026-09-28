# NEXT-14 员工D交叉review0.2

2026-09-28，员工D GPT-6-astra；同D-R12-01-20260928-E01。经理明确只读派发，已读主仓契约补充04；不修改B源码、不测试。结论：**N14-B04生产逻辑静态收口，尚有一项补充04直接验收断言缺口；不是独立动态验收或NEXT-14整体APPROVED。**

## 固定对象与本人冻结

受审B固定 `dd7a68d5cfa8b8ce24d4ce0bff0d64d0862fd297`，NoManCode tree `4ba8c49395502b57caf967f6527ba41476bc5ef2`，Rust tree `b027367d747e9194cc91cb61bbee60f2dd2ae428`，均通过git对象实际读取。比较起点0ec607f；未检出/合并B代码至本人树。

本人源码仍 `864e8d2df28988fd8556044620c2b8e799c58c60`，NoManCode `ad82fc69e93bfaae63dbebff3e0c0644fc07913d`，Rust `e1e172002af2e5350f18b4b2dd3dc5ada3c35ae8`；开始实核status/index空，no-build冻结持续。每次shell显式活动盘本人cwd，仅本人主仓0.2报告/身份新增；原0.1报告与12份原/发布日志未修改。B回报14测试全0属于B执行，经理待核，D未重跑或据此冒认动态通过。

## B04发现、裁决及修订复核

此前D对0ec607f发现：同Project同时预约两call，合法实际值i64::MAX与1使聚合超域；旧verify仅逐行，status会corrupt但open/recover接受。经理以补充04冻结结构与聚合分离，不丢已发生真实消费、不截断、不伪作not_sent；新准入、读取、开库和恢复统一聚合fail closed，既有在途仍可记录真实单笔。

固定dd7的 `NoManCode/rust-app/src/budget.rs:415` 起verify_structure保留配置、双表配对、逐行整数、请求历史及R快照校验；500–509 verify_persisted_rows先结构后逐Project调用status_calculate。该计算仍使用checked plus验证spent/held/occupied及下一R，超i64范围报corrupt。status_tx512调用完整校验，admit_check_tx通过status_tx进入；repository verify_schema_15及Store.recover也调用完整校验。finish_tx730改为仅结构校验，已claim调用不会因其他单笔造成聚合溢出而无法保存第三个可表示实际值；仍保持现有网络/预算同事务。这个拆分符合补充04，没有看到本次修订引入释放R、跳过结构/配对、重复加账或重新派发。

三在途反例测试budget.rs1307–1330确实先claim三个，依次完成i64::MAX、1，断言status/open/recover拒绝后再完成2；随后逐笔断言实际值和Finished，验证结构仍合法而整体聚合不合法。它没有通过修改原始值来假修复，也没有只用drop Arc冒称崩溃；这是纯Store结构测试，不宣称真实Host重启测试。

## 具体未闭合项

**补充04测试缺口，非已确认生产bug：** 上述1307–1330测试没有实际执行聚合溢出后的network_admit。建议B在第三call完成前或后增加直接准入调用，精确断言NetworkErrorKind::BudgetCorrupt/dispatch NotSent，并核network_calls与budget_calls数均不增加、旧三笔内容不变。该项对应补充04明确的“读/open/recover/admit拒绝”，不能单用status失败替代admit证据。生产调用链静态已接入status_tx，当前未见可绕过的产品反例。

## 请求指纹与coverage

budget.rs428起读request_digest，重算规范ConfigureBudget序列化的SHA256并与SQL持久值比较；同时原key、SQL Project、结果snapshot、revision连续历史、当前配置及原R校验继续保留。配置事务写入指纹与请求/回执/策略在同一commit；当前新增字段没有以同名列存在代替严格DDL校验。反例测试1333起将指纹改为64个0，精确断言status为Corrupt并拒开库，非仅长度校验。

policy234起补boundary不得超过network_calls全表MAX(rowid)，可堵住抬高边界后将新调用缺账伪装unmanaged的无依据高水位状态；rowid<=boundary有账及rowid>boundary无账原校验保留。指纹不是防恶意同步重写整个DB的密码学保证，报告没有扩大此承诺。

## schema15数据保全与限制

repository.rs3453起budget_requests DDL新增request_digest NOT NULL及固定长度；verify_schema_15仍精确比较规范DDL，随后检查持久结构/聚合与foreign_key_check。apply_schema_15依schema14 marker/version，三表创建、完整校验、插marker与更新user_version在同一外层事务，失败不独立补表或推进版本。

新增budget_schema_tests（3533起）读取所有表的 `rowid,*`，保留SQLite原Value类型；历史fixture非空Project、config sentinel、opaque blob `0001FFFE`、插件rowid37/历史请求rowid71均明确。成功预期只加三空表和一条新marker，旧表逐值及rowid精确比较，重复开库不改值；marker插入失败trigger断言两次失败不新增表且版本14，移除trigger后才迁移；双线程open通过屏障竞争并核同一结果。DDL/marker/version/孤儿反例均核开库失败且行快照不变，没有删除原断言让迁移通过。

证据范围：historical14由当前schema15数据库受控拆除预算三表/marker并降版本构造，并非运行旧版二进制创建；本组旧network_calls/Tasks等表可为空，不能把全表逐值比较称作每种历史业务都有非空fixture。生产迁移只增加预算表/marker/版本，静态未见重写旧业务行。更多非空网络历史和真实Host停点属于B/C完整U01/U07验收，不由本只读review代替。

下一第一操作：经理把上述admit断言缺口交B原owner补证后，结合原始日志核验做最终裁决。本人继续源码/no-build冻结；本0.2报告和身份完成后冻结供归档，等待经理新明确review范围，不自行测试或跨树整合。