# D-R5-01 第1.1轮只读复核与路径勘误

2026-09-27，员工D/GPT-6-astra，执行ID D-R5-01-20260927-E01保持。本轮按经理明确派发只读复核B/C固定候选，未修改产品源码，未运行构建、Clippy或测试；D候选仍abfb64d干净冻结。原始日志、第1.0报告及manifest不覆盖。

## 第1.0路径勘误

第1.0报告及当时消息将唯一Clippy位置写成store.rs:968-978，属于文件路径误记。原日志d-r5-clippy-01.log实际指向src/repository.rs:968的hydrate_network_ack侧表校验，lint为collapsible_if、退出101；归属B及失败事实不变。经理核对纠正后，本轮已只读核实。B在f282188已修该结构并报告其限定Clippy0；D未复跑，最终由C标准门禁覆盖。

## D03限定闭合复核

固定B f28218815ee7280a6eca77d20c685e085bb47381。repository.rs:2136-2159增加标准DDL独立内存库，逐表比较PRAGMA table_xinfo返回的列名、声明类型、notnull、default、PK序号、hidden；比较的是整列序列。TEXT NOT NULL与TEXTNOT NULL虽仍被旧空白归一化合并，但新检查分别看到TEXT/notnull1与TEXTNOT/notnull0，已不能通过。

原DDL字面量检查、active index完整定义比较和foreign_key_check仍保留在2167/2177/2181，新增元数据对照没有替换掉它们。tests/network_authorization.rs:733起真实重建TEXTNOT NULL伪表、插入NULL并查询确认，再关闭连接要求Store::open拒绝；不是仅检查字符串或构造一个本就无法插入NULL的夹具。结论：D03在本反例/源码范围闭合；不等同完整N09迁移验收或D独立测试通过。

## 此前有界只读结论

- C be51dd5的stdout失败修复保留已验证calls，闭管道真实CLI用例wait_with_output并断言exit1/原receipt/仅一次POST；真实Host崩溃用例child.kill+wait，重启同data目录查询原ID为unknown，call行数与Provider请求数不增。未发现新阻断。边界是响应尚未完整时kill，不单独证明完整响应读完与finish事务提交之间的更窄窗口。
- B200964的D02完整绑定回放校验和FK合法跨subject错链反例闭合；D01原activeifnotexists字面量案例闭合，但空白融合TEXTNOT NULL被D继续发现并交经理登记D03，现f282188按上节限定闭合。

下一步继续冻结，等待经理对组合的最终C标准门禁与验收；不重复运行已授权之外门禁，不跨写B/C。
