> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 C 第 1.2 轮复审：通过

日期：2026-09-23

审查人：项目负责人

结论：**APPROVED（现有 HTTP/SSE 传输契约范围）。C-R1～C-R5 全部闭合，无需继续提交本轮返工报告。**

依据：[第 1.2 轮报告](<../../提交报告/第一轮/员工C（HTTP与SSE传输契约 第1.2轮报告）.md>)、[第 1.1 轮复审](<员工C review 1.1.md>)、[原任务单](../../任务/第一轮/员工C提示词.md)。

最终交接：[员工 C 最终审查](<员工C 最终审查.md>)。

证据：[本轮审查附件](员工C第1.2轮审查附件/README.md)。

## 1. C-R5 的闭合依据

与第 1.1 轮实际审查源码逐项比较，`http_contract.rs` 只改变 H7 的三个位置：

1. [Provider 进入通知](../../../../NoManCode/rust-app/tests/http_contract.rs#L1009) 改为 `started.notify_one()`，接收方稍后等待仍可消费保存的 permit。
2. [entered 等待前](../../../../NoManCode/rust-app/tests/http_contract.rs#L1078) 增加有 3 秒上限的 yield 循环，先确认 calls 已增加，再创建 entered 等待，固定了上轮反例的“先通知、后等待”顺序。
3. [释放通知](../../../../NoManCode/rust-app/tests/http_contract.rs#L1107) 改为 `release.notify_one()`，与一对一、一次性握手一致。

H7 使用单线程 Tokio 测试，Provider 从 calls 加一到 started 通知之间没有 await；测试观察到 calls 后才等待 entered，确实覆盖通知先发生的交错。没有删除握手、busy 或完成断言，没有增加长 sleep，也没有把上轮整份探针复制成重复测试。

当前正式 H7 实际通过，仍验证消费 SSE 事件并断开后任务 busy、释放 mock 后 completed、Provider 总调用为 1、重连 seq 严格增大。**C-R5 的最后一项闭合。**

## 2. 范围与已有结论保持

`server.rs` 与第 1.1 轮审查附件 SHA256 完全相同：

`65031F6823F38E0AB9D3D94885CB0929A15EFD72BC3F1AFA0A0BAF2391ADC788`

本轮没有扩大生产修改。C 的契约文档补充了已接受的百分号例外：一次解码后参数名仍含字面 `%` 也拒绝，例如 `other%25=1`。这修正了“所有未知参数都忽略”的过宽描述，没有新增行为。

| 原问题 | 本轮核对 | 结果 |
| --- | --- | --- |
| C-R1：编码 after 绕过校验 | 原编码重复和非法值 HTTP 探针再次通过 | 闭合保持 |
| C-R2：存储一致性错误误报 400 | 原 Run 身份损坏探针返回安全 500，health 故障也通过 | 闭合保持 |
| C-R3：Path 错误纯文本 | 原非法 UTF-8 路径探针返回 JSON 400 | 闭合保持 |
| C-R4：坏 kind 编帧 panic | 同一流正常发送后注入坏 kind/JSON，均得到一次 error、正确 after 和 EOF | 闭合保持 |
| C-R5：测试证据与 H7 丢通知 | H7 强制晚等待正式回归通过，H8/H9/H10 与 health 断言保持 | 全部闭合 |

C-R1～C-R4 本轮未重写；重跑历史探针用于确认与当前 B 存储/执行快照的集成行为。没有将已认可的非阻断建议升级成新的返工要求。

## 3. 实际检查

审查对象为 HEAD `8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5` 加当前有效未提交修改。源码隔离副本：

`./.local/temp/fufu-c12-review-d94f190c6f`

使用真实 PowerShell 7.6.5 和项目 build.ps1 配置 MSVC。四项正式门禁在加入额外探针前完成；复用负责人空闲的独立编译缓存，没有使用、清理或改动员工 target。

| 检查 | 结果 | 附件 |
| --- | --- | --- |
| `build.ps1 -Action test` | **99 通过、0 失败、1 付费 live 忽略，退出 0** | review-test.log |
| `build.ps1 -Action fmt` | 退出 0，无输出 | review-gates.json |
| `build.ps1 -Action clippy` | 退出 0 | review-clippy.log |
| `build.ps1 -Action wasm-check` | 退出 0 | review-wasm-check.log |
| 原 7 项 HTTP 审查探针 | 7 通过、0 失败，退出 0 | review-probes.log |

99 项构成为：43 lib、4 adversarial、9 final_acceptance、11 http_contract、10 runtime、17 session_turns、5 protocol。H7 位于 11 项正式 HTTP 测试中；额外 7 个探针不重复计入 99。

未执行付费 Provider 测试，仅使用本机 mock 与临时数据库。仍有既有 proc-macro-error2 未来 Rust 兼容提示，不是本次门禁失败。记录的 7 个 C/B 关键源码文件在结束复核时均与快照哈希一致。

## 4. C 报告中的全量失败如何处理

C 报告记录了其执行时 B 的中间态：session_turns 为 16 项，T9 触发 `Cannot start a runtime from within a runtime`，fmt 在 B 文件存在差异。报告明确退出码、没有越权修改 B，处理方式正确。

负责人复核时 B 已继续修改，当前固定快照的 session_turns 是 17 项。T9 已将独立 Runtime 的创建、block_on 和销毁放入普通工作线程，外层测试异步等候，停止和 join 通过 spawn_blocking 完成；本次 T9 和全量 fmt 都实际通过。

因此两份结果对应不同快照，并不冲突。新结果更新到本 review 和最终审查，保留 C 原报告作为当时记录，不要求 C 为他人的后续修改重写历史报告。**这只是本次集成门禁事实，不代替 B 第 1.2 轮的功能复审。**

## 5. 最终范围与下一步

- C 第一轮现有 HTTP/SSE 任务结束，C-R1～C-R5 全部关闭，不再要求第 1.3 轮返工。
- 放行现有读取、输入与错误响应、安全拒绝、Run/Session SSE 游标、续传、范围隔离及开流后故障处理；既有 Run 幂等兼容保持。
- 新连续 Turn HTTP 写入口、B 应用错误映射、CLI/UI 接入不在本次批准范围。等 B 契约复审通过后，由负责人发独立联合任务。
- H9 阈值处单独观察 HTTP 等待、测试读帧器完整消耗注释后的缓冲，沿用上轮非阻断补强项；以后扩展相关测试时处理，不为此重开本轮。

本次负责人只读取、隔离复验和写审查文档，未修改共享 Rust 实现/正式测试、员工报告，未提交或发布。生产实现与正式测试修复由员工 C 完成。
