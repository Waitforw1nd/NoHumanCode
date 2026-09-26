# 员工 B review 1.0：工具会话与生产网关

经理 GPT-6，2026-09-27。任务 B-R4-01，执行 B-R4-01-20260927-000811；员工实际模型 GPT-6-astra。结论：**APPROVED（B 五文件实现与定向证据范围，供联合验证；NEXT-05 整体仍待 C 联测和固定组合五门禁）**。

受审固定候选 `8df22e3664b6d1a773661a2b3879ee2b284ec853`，Rust tree `d2b2d204598a3791d7963a2989fe0376e17c6004`，基线 `5cf74319ea3b37af9c27821a6eaac944241473e4`。经理核对[正式报告](<../../提交报告/第四轮/员工B（工具会话与生产网关 第1.0轮报告）.md>)、实际 domain/store/engine 及新旧测试差异、原始日志；另参考[D 固定差异只读支持](../../../员工D/提交报告/第三轮/NEXT05联合只读支持-B-8df22e3.md)。经理未修改产品源码或复跑正式产品测试。

## 已闭合与依据

- RunContext 使用 SQLite 一致读事务读取真实身份，按首事件提交顺序选择 latest；缺失/未映射/损坏分型，保留数据库操作错误，HTTP 由 C 映射。两个 Store 定向测试覆盖真实关系、旧事件晚到及损坏，未虚构 ID。
- 新 Turn 仅追加 completed 单 Agent Chat；Engine gate/scope 检查后由 Store immediate 事务重查前序、权限、路由、工作区、完整消息前缀。旧 key 在资格检查之前回放，无新模型调用或审批。原有无工具测试的权限排除改为依赖排除，Host workspace 变化改为明确拒绝，符合新契约而非删安全检查。
- 工具历史闭合、调用 ID 唯一及原 Task/Turn/Session 结果来源逐层核验；继承调用不能在后继借用新审批。所有历史 Task 的未决审批/change/restore 阻断新 key。Repository 原有审批来源/finished 事件验证仍生效。
- **D-N05-01 已关闭**：不再将旧 interrupted 结果文本视为副作用已明确；受控调用缺少同源审批即 UnresolvedEffects。write/command 数据形状夹具验证精确错误和零新增对象/模型请求；这不是运行旧二进制的崩溃实测。
- complete restore 的 Host user-role 事实与事务快照使用同一 helper；不改旧工具结果，不抬升用户输入权限。真实新写 before 摘要及再次 restore 字节验证通过，旧 Task 封存保持。
- Engine definitions、准入与最终 lease.execute 连接同一生产 runtime；批准等待期间不持 lease，acquire 同步回调进行真实 claim。停止先胜零 claim/文件效果；claim先胜仅原lease完成。原 workspace gate、prepare、finish事务与 unknown 不重试保持，无 runtime 锁跨 await 或 Store→runtime 反向嵌套。

## 验证和证据边界

B 定向 lib80/session19/tool-session10，共109通过；AP31/AH14/runtime10/W21/crash6/WHTTP3，共85通过；all-target Clippy、fmt、diff检查退出0。经理核6份原始/发布日志（含命令账）双端SHA-256全部一致。早期E0004依赖失败、N04错误明文夹具比较失败、Cargo目录误用均保留，后续通过不改写失败事实。组合后check0无独立日志一项按报告披露，不作为可复核的独立归档证据。

N03～N07、N09在B职责内通过。N07新增屏障精确证明restore先排队、send实际Pending后消费恢复事实；same-key join仅证明收敛，不能称双DB强屏障，后者沿用已保留且回归通过的session_turns测试。N09重复claim测试只断言is_err；错误类型UnknownResult为源码及夹具核对，未冒称独立typed断言。其他停止/claim及文件计数断言实跑通过，不据此扩展无关返工。

N01/N02完整CLI、N04 CLI链、N10～N13和N14最终联合门禁由C完成。B 8df22e3已在C干净冻结bc0237a上由经理无冲突组合为`9b76898f1a4f2162d53af5487d58b9191b575417`；组合不是验收。B源码继续冻结，不重复跑最终门禁。main当前仍为先前已验收Workspace产品。

归档检查：新发布副本两份日志多余尾部空行被diff-check指出；经理仅裁掉本轮未归档发布副本空行并更新manifest发布哈希/转换说明，原始日志不改。D新支持更新引起身份历史换行变化，已核正文一致并保留HEAD历史后缀原字节。
