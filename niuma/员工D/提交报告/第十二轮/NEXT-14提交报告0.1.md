# NEXT-14 员工D提交报告0.1

2026-09-28；员工D GPT-6-astra；D-R12-01-20260928-E01；**提交待审，非最终验收**。

## 版本、范围及实现

完整开发基线3a94ff41b7fb44e6eae225e6d8934b081e1ae8b4，两源码tree5fc44d1d2c4f65790cab94c376c59b1466e88980/e4a5012dea4d2d0f7acb352de496a66f61bb2097。本人树../nhc-d-provider-usage、codex/d/provider-usage，完整读取v1.1契约/补充01、执行登记和正式任务，基线实核clean/index空后真实回接收。PRE已APPROVED结项，未恢复旧D-R11。

首纯classifier提交6ff19bbd472d164ed18155c2ab98afec647fa539；经理合入B真实lib导出后，本人先实核1d2ba04931a090025a4727e1bd2220942808fc36 / NoManCode7ffee10848165148161826d32aa696355fe6a4be / Rust1ea043c8fa85b8bb4137ffa4dc08af452be8cb7a，clean/index空后继续；没有改lib或伪造依赖。

本轮固定候选 **864e8d2df28988fd8556044620c2b8e799c58c60**；NoManCode **ad82fc69e93bfaae63dbebff3e0c0644fc07913d**；Rust **e1e172002af2e5350f18b4b2dd3dc5ada3c35ae8**。本次全部验证的前后SHA/two tree与status在metadata内一致，未在构建中改源。

唯一改动文件：`NoManCode/rust-app/src/provider_usage.rs`（新）、`src/provider.rs`、`tests/provider_usage.rs`（新）。未改provider_transport.rs、lib.rs、budget/Engine/Store/schema/HTTP/CLI或旧fixture。源码暂存与提交仅本人树明确文件；主仓index/跨树合并仍经理。

- public TokenUsage/UsageObservation/UsageInvalidReason/classify_usage完全按v1.1类型；仅非负JSON整数且<=i64::MAX、checked分项和等于total为Valid。null Missing；错误结构、缺项、字符串/小数/负数、越界和合计不符为固定Invalid枚举。
- Decoder私有typed锁存，重复规范三整数相同接受一次、不相加；不同/partial/invalid永久锁存，后续valid或null不能修复。pub usage继续提供Value兼容，complete与TransportError签名保持。
- tuple中的usage只含规范三整数、Null或固定内部invalid标志，details与任意Provider文字不保留。内部标志只映固定枚举；携标志又携三整数不能伪装Valid。
- 网络成功与缺失/错误用量分离，消息仍成功返回。已发送失败/流错误不挽救部分usage，不改变旧Unknown/Response语义、不重试。Provider本身不操作预算DB，也不改变合法工具授权。

## 真实验证与失败保留

Rust cwd真实rustc/cargo1.98.1。当前机器MSVC通过卸载注册表安装路径及既有目录发现，只读使用，未安装/更新。所有target/TMP/TEMP/log只本人活动盘树.local；既有fixture使用的rust-app/target目录也在本人树。环境仅验证进程debug0/incremental0/jobs2；旧树与旧证据未清理。

| 固定864e8d2实际命令 | 结果 | 原日志标签 |
| --- | --- | --- |
| cargo check --lib --locked | exit0 | check-attempt1 |
| cargo test --locked --test provider_usage -- --test-threads=4 | 7通过/0失败，exit0 | usage-attempt1 |
| cargo test --locked --lib provider:: -- --test-threads=4 | 17通过/0失败，含1个未设置子进程环境直接返回的proxy helper；扣除helper为16，exit0 | provider-attempt1 |
| cargo fmt --all -- --check | **exit1**，仅合入的B首版budget.rs五处格式差异；已报经理及B原owner，未越权修复 | fmt-attempt1 |
| cargo clippy --locked --lib --test provider_usage -- -D warnings | exit0；限定生产lib与新integration目标，不冒充全workspace/all-targets | clippy-attempt1 |
| rustfmt --edition 2024 --check src/provider.rs src/provider_usage.rs tests/provider_usage.rs | exit0，原输出0字节；实际Rust cwd版本，非workspace门禁替代品 | fmt-owned-attempt1 |

本轮两个测试结果块合计24通过/0失败/0忽略，其中1 helper，扣后23项实质测试；不与B/C或最终五门禁拼计。新增测试函数为7个公开classifier/Decoder测试和4个真实mock传输测试；旧Provider回归同次限定组件执行。没有付费live或最终五门禁（仅C所有权）结果。

U05：纯解析覆盖零、i64边界、u64/超过u64、三字段逐项缺失和错误类型、checked合计；Decoder按1/2/3/17/大块切分含UTF8与CRLF，details剥离、相同重复、总数相同但分项不同冲突、partial不得拼合、invalid后valid不洗白、缺usage保留成功消息、invalid标志安全规范化。真实监听器+Provider complete覆盖HTTP chunked单字节帧、正常/零/边界/相同重复、12组missing/invalid/冲突序列，逐次断言实际请求次数1、消息成功与规范usage、发送include_usage和max_tokens128。共用生产ProviderClient/完整SSE Decoder，内部permit夹具仅供Provider边界专项，不冒称Engine网络授权或预算准入证明。

U06的D解析部分：真实mock先给有效usage但finish length/content_filter、缺DONE、HTTP429/500仍得到原错误且只请求一次，未挽救usage；旧组还覆盖真实HTTP截断、emit失败、响应字节上限、redirect不访问二目标、匹配失败0连接等。本报告不把它们视为预算释放/持久结算证据，后者归B。

DONE专项：Provider在完整SSE DONE后即返回，即使声明更长HTTP body且连接保持不关闭；这是已消费协议数据保证，不冒称验证全部尾随网络数据。

## 证据

原件位于 `../nhc-d-provider-usage/.local/logs/`；六次动作各log/json，共12原始文件保留。发布副本及独立原/发布SHA256和长度见[manifest](证据/864e8d2-provider/manifest.json)。仅发布副本将本人工作树与运行时发现工具链绝对路径改为相对逻辑路径；原件未改，metadata内log_sha256仍指原日志。全部实际UTC起止、命令、exit、before/after SHA/tree/status及工具版本可查对应json，不手改失败结果。

原workspace fmt1完整保留。没有编译或测试失败；fmt唯一失败不能包装成全通过。尚未执行最终固定组合检查和预算DB/HTTP/CLI联合验收。

## 边界及下一步

本机只严格分类Provider报告数字；R/max_tokens都不是真实总tokens或美元硬上界。同一个JSON对象重复token键的入口级检测按经理明确范围不扩展，Value已经折叠键；多SSE usage块重复锁存已覆盖。失败usage一律保守未知，无部分结果退预约。预算持久、unknown冻结、overrun、Task安全投影、配置及跨Host恢复由B集成，HTTP/CLI和最终门禁由C；D没有将这些未执行事项写通过。

当前本人源status/index空、全部构建结束且no-build，已向经理开放源码冻结整合窗口。无已知D组件阻断；B首DTO格式待原owner后续修订，不跨权。下一第一操作：等经理固定review或合入完整B组合通知；核实际SHA/tree/status后按明确范围复核，不自行merge/重跑最终门禁，不自行APPROVED。本人报告/身份在本交付后同样冻结供经理审查与归档。