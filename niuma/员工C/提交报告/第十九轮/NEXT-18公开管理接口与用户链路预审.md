# NEXT-18 公开管理接口与用户链路预审 v0.1

2026-10-31，员工 C / 6.1 Terra。仅预审、未实施；路径、字段、语义均为建议，经理冻结正式契约。主线 HEAD `9b345e0699f615846495752813b367afd21d7733`，NoManCode tree `906328dea19bc45b0a29c8279b931144b08144cb`，Rust tree `7e9f77907c5df4712d2689dd22532579d3e01e15`。主仓已有各角色并行档案修改，未触碰他人内容；原 C 独立树保持冻结。本轮未构建、测试或 Git 写入。

## 当前证据与兼容约束

- `NoManCode/rust-app/src/engine.rs:998` 固定旧列表两项；`plugin_management.rs:69` 旧 ID 校验只接受 `builtin.files`、`builtin.shell`。同文件 132–180 以一张 `plugin_management_requests` 表按 key 全局查旧回执，先判同参回放/冲突再比较 revision，事务一起提交 desired 与回执。`engine.rs:1034` 回放不再次 reconcile，而 `current` 是另一次当前观测。全局域指这张插件管理账本，不推断与所有其他业务表共用 key 域。
- `server.rs:409` 是旧插件路由，`:512` 仅明确列出 Agent 旧诊断和插件等认证读路径；新增 GET 路径必须纳入认证，不可因默认 GET 分支漏鉴权。`:699` 旧 Agent 状态拒绝任何 query。`:727` 旧写入已有严格对象与单一幂等头；旧接口 DTO 和可访问 ID 边界保持，不能因为底层账本 ID 扩展而把新 ID 暴露给旧入口。
- `cli.rs:1670` 对 Agent 旧响应字段集合、固定 ID/service/version/scope 和状态关系进行校验，且 `generation != 0`；当前 `agent_runtime.rs:246` 从 1 开始。拟议 desired=false 零激活启动时若返回 generation=0，会让已有 CLI 拒绝原形状响应。建议禁用初始状态亦保证非零代次，同时真实 inactive、无 effect、admission 关闭；若底层无法保证须经理明确兼容迁移，不能只说“字段未变”即可。不得为满足校验而伪造激活。
- `provider_runtime.rs:190` prepare 与 `:223` acquire 分阶段，`:284` 代次变化、`:319` 状态在当前进程；Provider 的 in-flight 与 effect 撤销不等同 Agent 排空（`agent_runtime.rs:301`）。不得用同一套跨类型状态不变量硬套。`cli.rs:1292` 常用 HTTP client 配置禁止 retry 和 redirect；新写命令须走同等配置并用真实断线用例证明，而非只看代码。

## 最小接口建议，待裁决

建议独立 `/api/runtime/components`：GET 列表、GET `/api/runtime/components/{id}`、PUT `/api/runtime/components/{id}/enabled`；只允许 `builtin.chat-agent`、`builtin.provider-xpeach`，未知及旧 files/shell ID 固定 404。CLI 建议 `runtime component list`、`runtime component status <id>`、`runtime component set-enabled <id> <true|false> --expected-revision <n> --key <key>`，具体参数按项目现有风格定。既有 `runtime agent status` 与 `/api/runtime/agents/chat` 原样保留，旧 plugin 命令及列表只含 files/shell。各端点列表固定集合，顺序明确但两项不是跨组件原子快照；如果不是在同一临界区获取则禁止标注全局同一时刻快照。

请求体严格仅 `{enabled: bool, expected_revision: u64}`（实际上限 `i64::MAX`），单一合法 `Idempotency-Key`；无 query，拒绝未知字段、重复字段、错误类型、溢出、非对象、缺失、重复/合并 key 和不支持的方法。注意 JSON 先解为通用 Value 会丢重复字段证据：从原字节按严格对象解析，再按每一层封闭结构校验；CLI 对服务端响应也需原始字节检测重复字段，不能先 Value 合并后宣称拒绝。响应限长、JSON content-type、固定 status/code/message、无未注册字段、无密码/密钥/URL/任意异常文本；先认证并统一校验 Host、Origin、Sec-Fetch-Site，安全失败的最终错误顺序请经理冻结。

建议 status 包含固定 `id`、`kind`、`capability`（Agent 为 strategy，Provider 限 Completion；不可表述为 Models/Balance 控制）、`desired:{enabled,revision,updated_at,scope:host}`、`current:{scope:process,...}` 与 `converged`。current 使用各类型封闭 tagged DTO，字段和状态转换以 B 契约裁决，不允许任意 Map 或 schema。`converged` 必须由正式状态矩阵定义，尤其 disable 后 Provider 存量 in-flight 是否影响收敛；展示是瞬时观测且不承诺跨进程广播。失败枚举只能白名单、绝不回传原异常。列表/状态只是投影，不新增独立业务状态或账本。

写响应建议 `{receipt:{id,enabled,previous_revision,revision,committed_at}, replayed, current:<当前status>}`；回执只证明事务接受，不能声称生效；回放时 receipt 原值不变，而 current 可能不同。校验回执 id、布尔与请求相符、previous_revision 与请求相符、revision=previous+1 且不溢出；current 只校验自身类型/一致性，不能强迫等于旧 receipt。若提交后 current 读取失败则固定 internal/unknown，客户端不能宣称未提交；用户可查询当前并使用原 key+原参数手动回放以取原回执，不自动重试或换 key。是否增加只读按 key 查询需经理决定；仅查 current 无法证明某次写是否提交。

建议固定 `runtime_invalid` 400、`runtime_not_found` 404、`runtime_revision_conflict` 409、`runtime_idempotency_conflict` 409、`runtime_busy` 409、`runtime_unavailable` 503、`runtime_internal` 500；响应字段如旧接口固定 `{code,message,error,retryable:false}`。Busy 明确“未提交本新请求”，不能与写后 unknown 合并；旧 key 同参回放和异参冲突先于 Busy，避免在 draining 期间否定已提交结果。Busy、冲突均无新回执/无 revision 改变；新 disable 在 draining/transitioning 期间是否允许、同状态新 key 是否产生 revision 由经理冻结。传输断开、超时、错误体无效/截断及可能写后 internal 均按写结果 unknown 呈现，不能自动重发；不能把服务返回的已验证 Busy 误报为 unknown。

## 真实公开链路与反例建议

1. 基于 `tests/first_user_workflow.rs:76,216,326` 的真实 Host 子进程、独立临时空库、真实 CLI 和本机假 Provider 计数，沿公开 HTTP/CLI 初始化并读两个初始 revision、desired 与 current。断言旧插件列表仍仅 files/shell，旧 Agent 诊断字段及旧 CLI 可用，新管理面不得透出 secrets。禁用 files/shell 后无工具 Chat 的行为必须单独实际验证，不靠状态推断。
2. 分别使用新 key+正确 revision 禁用 Agent 和 Provider，验证 receipt、当前准入实际关闭、旧 Agent 诊断可读；kill+wait Host 后以同 DB 重启，两个 desired=false、Host 可管理和恢复既有任务事实，运行态不激活、无 effect；再经公开 CLI 用新 key+当前 revision 分别启用，证明真实 Agent 策略及 Completion 本机假 Provider 调用恢复。对各步骤记录调用计数、Task/receipt/预算与对应响应，空库不预写数据库或用内部 Engine 代替公开入口。区分独立关闭 Agent 和 Provider，避免前者遮蔽后者；异步 Task 已成功创建时只断言后续终态而非称创建失败。
3. 先保存 disable receipt，启用后以旧 key/原参重放：原回执字节或字段一致、`replayed=true`、current 显示新状态、Provider 调用/生命周期次数无增加；同 key 改 ID、enabled 或 expected_revision 与 files/shell 交叉复用均 409，不动任一 revision。用过期 revision 新 key 409。空库及迁移数据库都覆盖，已有 files/shell 行和旧回执不变。
4. 保持真实 Agent 在途，disable 后新请求关闭、旧 lease 完成；draining 时新 enable 返回 Busy 且无提交，已提交 key 重放仍成功。Provider 分别覆盖 prepare 后禁用导致 acquire 失效、已 acquire 可完成；锁/回调和跨 Store 准入竞争由 B/D 独立覆盖，但公开层须观察实际拒绝与账本一致。对另一 Store/Engine 提交 false 的情形分别验证新 Agent 与 Completion 最终准入，不声称实时跨进程广播。
5. 假服务收到完整 PUT 后断线/截断响应，计数精确为一次，CLI 固定 unknown、不输出秘密、不自动重试；由操作者重新显式使用同 key/参回放，服务若已提交返回原回执。验证 HTTP 鉴权、Origin/Host/site、方法/query、重复字段/键、非法类型/边界、错误映射；假服务发送未知字段、重复字段、未知枚举、跨 ID/能力、矛盾 current、错误 status/code、嵌入 token、超长/截断体时 CLI 安全拒绝，写入结果按 unknown 处理。
6. 独立验证 Provider disable 不禁止 Models/Balance；route check 已完成 Models 后 Completion 被拒绝时保持已有网络回执与预算事实、不编造完成或重试 unknown。禁用启动与激活失败 fail-closed、迁移故障及真实进程崩溃的归属与停点交 B/D 正式任务确定，不能以测试替身声称完整验收。

## 待经理裁决

路径/CLI 命名、DTO 精确类型与错误码；Busy 优先级与同状态新写 revision；两类 convergence 定义和跨进程观测边界；旧 Agent 非零初始代次；是否提供按 key 只读回执查询；公开完整链路中假 Provider 对 Models/Balance 的可观测机制与权限/预算设置。以上仅建议，未新增产品实现、测试结果或验收结论。
