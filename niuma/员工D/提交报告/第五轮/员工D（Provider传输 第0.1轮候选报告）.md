# D-R5-01 Provider transport 第0.1轮候选报告

2026-09-27，员工D / GPT-6-astra，执行ID D-R5-01-20260927-E01。此为已实现、未编译的依赖候选，不是功能通过或最终验收。

基线c5430b166c6c35307f60b75ce5a8738d44f9c2e1，Rust tree b549de2e18d39c8e69e6607b9f430974895ee4b9。独立树../nhc-d-provider-transport、分支codex/d/provider-transport，源码/target均在迁移后E盘；接收status空。本人仅操作本人独立index/提交，经理串行合依赖，共享index及旧树/备份未改。

首个真实接口提交77d08e612aab5c7b866d37841e1b6c0b05685a89，仅新增src/provider_transport.rs，已由经理作为未编译依赖合B。后续候选43fc6ee448a1f87399180a8d31bfdf6cb5b24d81，Rust tree5a00e16c9eb4eed0cd8f8defbad421955eac5905，status空；累计只改src/provider_transport.rs和src/provider.rs。

## 已实现但待真实依赖验证

- 规范目标唯一构造器，原输入歧义拒绝，规范origin含effective port，保留base path；私有NetworkTarget及精确accessors。
- 不可Clone/无Debug的单次permit绑定URL/method/key/model/account user/call ID；纯validate_permit_inputs供Host claim前预检。构造器pub(crate)，无公开测试旁路。
- 私有reqwest client显式no_proxy、redirect none、reqwest0.12.28 retry::never。Provider入口按值接permit并核第二对象target/model/user，最终出站授权字段均取permit；send额外核JSON model一致。
- JSON响应逐块累计<=4,000,000字节；完整非2xx、完整无效响应、发送后未知与发送前Invalid静态分类；无原始错误source。SSE需要finish_reason及完整DONE，截断/emit失败/格式失败为Unknown，不再忽略finish后的读取错误。
- 私有unit fixture：五redirect状态双真实TCP端点及POST次数、错目标/model/user/purpose零连接并同端点正对照、账户header、精确4MB与超限未EOF、HTTP/坏JSON/截断结果、SSE双结束和emit失败、隔离代理环境子进程与专用代理正对照。测试源码存在不代表已通过。

## 验证事实

实际单文件rustup run stable rustfmt --edition 2024 src/provider.rs src/provider_transport.rs退出0；git diff --check退出0，只有Git行尾归一化提示。原始日志在本人树.local/review-evidence/d-r5-diff-check-01.log，原样发布副本见[日志](证据/d-r5-diff-check-01.log)。两个SHA256见下。

编译、unit测试、真实Engine集成测试均未执行：当前D树缺B的lib/Engine和C server签名接入。按经理指令先固定候选，再由经理串行组合真实依赖回D进行定向运行；没有临时stub、没有修改B/C文件、没有以旧crate编译冒充新模块验证。最终五门禁唯一执行者C。未release、付费live、push。

## 下一步

保持候选源码干净，先只读自审。经理回合真实组合后运行定向unit、补tests/provider_network.rs公开Engine preview/confirm测试并验证。最终报告另开轮次，包含实际失败、修正、命令/退出码与双hash；当前不作通过声明。
原始SHA256：49836261181a1c220481b5716d5539e4bc0f530bf860af02c160fb55b8cbcd90

发布SHA256：49836261181a1c220481b5716d5539e4bc0f530bf860af02c160fb55b8cbcd90

