# NEXT-18 C-R19 首审返工静态候选

2026-10-31，员工 C / 6.1 Terra，执行 ID `C-R19-01-20261031-E01`。已读[经理首候选审查](../../审查记录/第十九轮/NEXT-18首静态候选审查.md)，仅原授权范围解除源码/index冻结，no-build 保持。原 `4b47a51b9a020cdfb7353f7d3efdb2703187a91b` 首候选及报告保留；本次增量提交 `273895d09908ddc5a7eb0ac6152d3a7494b77e66`，NoManCode tree `fb7313223c47450274e11db02a5f68f854d499a7`，Rust tree `816c5d24c7aef27fcd98ba071f8e210c546e8efb`。四个授权源码/测试文件；文档未改。提交后 status/index/unmerged 空；`rustfmt --edition 2024 --check` 与提交前 diff check 通过。未构建、编译、运行测试或取得动态证据；B 草稿尚未固定合入。

## 静态修订

- R01：两类 transitioning 禁止 admission；Agent 补旧诊断所要求的 draining/active/transition/in-flight/effects 关系，Provider 不套 Agent 排空限制。假服务测试增加非法 transition 的读取拒绝、写入 unknown，以及 Agent draining、Provider 禁用后仍有 in-flight、transition 旧 effects、完整 u64 的正例。
- R02：新增两个开关各自禁用时创建真实异步任务、验证失败终态、同 key 重放原任务及无网络调用/预算增长，然后各自恢复 mock Completion。已在途 Agent disable、Busy 不消费 key、旧 receipt 回放、放行旧任务后用同 key 启用；插件四 ID 全局 key 冲突、过期 CAS、同态新 revision 及 replay 不改变代次。空库重启故事增加任务 `tools=false`/空 write scopes 及实际 Provider 请求 tools 校验。
- R02 断线：新增受控代理先完整读取 PUT 并转发至真实 Host，保存 Host 返回的实际 receipt，随后发送截断响应；测试断言 CLI 一次发送和 unknown、Host 真实 desired 改变，并由用户显式使用原 key/参数向 Host 回放比较原 receipt。不是硬编码回执或仅计 handler 入口。
- R03：真实 Router 静态用例增加 HEAD/方法/Host/GET body、缺失/重复/合并幂等头、错误 content-type、非对象/缺字段/负数或溢出 revision；CLI 用例增加嵌套重复字段、超限、错误 HTTP/code/字段类型、human 输出及参数 exit。既有测试覆盖安全头、鉴权、Origin/site、服务端 PUT 限额与安全拒绝。
- R03：现有 Models 已完成后拒绝 Completion 的用例按返回 call_id 从公开 `network call` 查询并比较持久回执；新增独立假 Balance 服务、真实账户绑定/授权、关闭 Completion 后的公开 Balance 调用和计数。

## 证据边界与下一步

以上是静态测试代码和预期，**没有运行结果**。新 Host 进程/代理、Budget 和 Balance 场景是否满足实际 B 接口与运行行为尚未证明，临时夹具、请求/回执及 kill+wait 均未实际产生；不得据此宣称 M04/M06/M12–M14 通过。B 状态机最终候选需与新增中间态约束核对；若其真实合法状态不同，交经理裁决，不能擅自降低断言。下一第一步：请经理增量 review 并等待 B 冻结、经理串行整合；实核新三 SHA 后等待精确组件放行。无授权不构建，主仓 index/跨树整合仅经理。
