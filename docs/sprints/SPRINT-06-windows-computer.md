# Sprint 06 · Windows Computer

## S6-T1 · 观察指定应用与等待桌面事件

从 [Baseline 7](../BASELINE-CHANGELOG.md) 开始。现有 Agent 主循环已经会等待工具结束，MCP 也已按会话复用连接。记事本“另存为”场景缺少的是一个在工具调用之间持续有效的桌面目标。先交付这项能力，同一套观察和等待服务供模型工具与本机诊断命令调用。[Agent 主循环](../../src/engine.rs) · [MCP 会话池](../../src/mcp.rs) · [ADR-012](../ARCHITECTURE-DECISIONS.md)

| CLI 缺口 | 本轮决策 |
| --- | --- |
| Host 无法判断目标窗口是否仍在前台，工具也无法等待对话框 | 先实现 `computer_observe` 和 `computer_wait`，用 `coda computer watch` 在交互桌面离线验收。 |
| 控件操作尚无可复核的引用和动作回执 | 下一片在已观测的目标上引入 UI Automation 控件引用与审批。 |
| 截图缺少经过模型适配层的图片通道 | 自绘应用的视觉覆盖单独实施，先验证三种模型协议发送的是图片。 |
| `coda jobs continue/retry` 及聊天内 `/jobs` 原先创建了取消令牌，但未接入 Ctrl+C | 在 S6-T1 验收快照之后单独修复：中断后等待当前尝试写回，保留回执，未派发分支留待续跑。见[真实 Windows 控制台回归](../../tests/support/durable_ctrl_c.rs)。 |

### 本轮使用契约

操作者在启动进程前用 `CODA_COMPUTER_TARGET` 登记绝对工作区和一个已启动应用的 PID。仅在匹配的工作区中开放模型工具。前台属于目标时，观察可返回窗口类、标题和会话内的观察引用。焦点离开时只返回 `outside_target`。Host 存续期间以打开的进程句柄确认目标是否退出。每次启动新 Host 都按当时登记的 PID 打开进程，因此启动前应核对目标 PID，结束后清除环境变量。

`computer_wait` 支持 `target_foreground`、`dialog_foreground` 和带最近观察引用的 `changed`。等待有超时且可取消。先订阅事件再检查现状，可以处理对话框在发出调用之前就已打开的情况。唤醒后根据当前状态判断条件。标准 Windows 对话框的识别是这一片的边界，`dialog_foreground` 本身不能证明该窗体就是“另存为”。[WinEvent 消息循环](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwineventhook)

### 完整场景的后续交付

1. **控件观察与定向动作。** 先在记事本及其同进程保存框上识别控件的角色、名称、可用状态和支持的 UI Automation 模式。引用绑定目标进程实例、窗口与一次观察。每个写动作在审批后重新取得控件，复核所属窗口、前台、模式和可用状态；优先对该元素使用 `ValuePattern` 或 `InvokePattern`。失焦、窗口变化、元素失效时返回需要重新观察。仅在发送前查一次前台，再向系统当前焦点发送全局键盘输入，仍然存在误输竞态。
2. **保存与核验。** 在临时工作区使用已知的测试文本和新文件名。先等待保存框，刻意切到另一应用，验证旧引用无法在那里输入；回到记事本后重新观察，核对绝对目标路径再批准保存。沿用项目写权限、变更前快照和用量之外的工具执行回执；派发后结果不明时由用户确认。最后用现有 `read_file` 独立核对内容。该流程需要真实 Windows 桌面验证。[Microsoft UI Automation](https://learn.microsoft.com/en-us/windows/win32/winauto/uiauto-uiautomationoverview)
3. **图片通道。** 自绘控件需要图像时，让 Host 在单次授权范围内获取目标窗口图片，消息历史只保留有生命周期的附件引用。发送时由各路由适配成协议支持的图片内容块，限制大小并在能力不足时明确返回。例如 Anthropic 路由可把图片放进对应 tool_result 的内容块。每种协议都用回环服务核对请求体中的媒体类型、图像字节、工具调用 ID 和续聊顺序，避免截图进入文本截断逻辑或普通日志。[Anthropic 工具结果格式](https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls) [工具结果封装](../../src/tool_call_application.rs) · [协议适配](../../src/provider.rs)

### 验收

| 项目 | 服务器结果 |
| --- | --- |
| 格式、Linux 和 Windows GNU 全工作区全目标编译及 Clippy | 全部通过。Clippy 启用 `-D warnings`。 |
| Windows GNU / Wine 定向回归 | 124 个不同用例通过，包含 Baseline 7 的 115 项及本轮 9 项。 |
| Linux 全工作区测试 | 138 通过、39 失败、33 忽略。与 Baseline 7 同环境比较，失败用例逐项相同。 |
| 模型回环 | 登记的窗口状态和等待超时结果进入下一轮请求。等待期间模型请求数为零。无登记、错工作区、工具禁用时，伪造调用均遭拒绝。 |
| Win32 双进程窗口实测 | 在私有 Xvfb/Wine 显示器运行实际 `coda.exe`，依次返回主窗、标准对话框、外部进程、同一对话框、主窗和目标退出。切走时记录不含标题或类名，切回产生新的观察引用。 |

### Windows 本机验收

[Baseline 8](../BASELINE-CHANGELOG.md) 登记的是 `baseline-1` 加 S6-T1 累计补丁。`%LOCALAPPDATA%\Temp\coda-s6-t1-2739985d3e` 下的 `verification.log` 记录了补丁哈希、独立 worktree 的应用检查及原生门禁；`exit-codes.json` 中 `fmt/check/clippy/test` 均为 0。按 `test.log` 的各组结果合计 227 通过、0 失败、1 忽略（付费在线测试）。

| 真实记事本记录（相对上述证据目录） | 观察到的状态与等待结果 |
| --- | --- |
| `watch-031835255/observations.jsonl` | 从 `Notepad` 主窗开始，多次切出和切回；最后一次失焦后，配置为 60 秒的等待返回 `timeout`，状态仍为 `outside_target`。 |
| `watch-032130737/observations.jsonl` | 从 `outside_target` 切入 `Notepad` 主窗，随后匹配 `dialog_foreground`（类 `#32770`）。两次离开对话框再切回，每次切回都有新的观察引用；之后回到主窗、失焦，最终收到 `target_exited`。 |

两轮 `watch` 的退出码均为 0；失焦记录都不含标题或窗类。`#32770` 仅表明观察到目标进程的标准对话框，不能据此认定其用途或保存结果。本轮只验收观察与等待，未执行控件输入、保存写入或读回文件核对内容。

### 基线后 CLI 修复：在探索任务中按 Ctrl+C（2026-11-26）

两条回归分别从 `coda jobs` 和聊天内 `/jobs` 发起 `continue` 与 `retry`。在独立 Windows 控制台中，等回环服务实际收到请求后发送 Ctrl+C。修复前，命令行退出时任务仍为 `running`，聊天则直接退出。修复后，已完成分支不重跑；已发出请求的分支在下次启动前已写回 `unknown`，普通 `continue` 不会重发；尚未派发的分支保持 `queued`。显式 `retry` 产生独立回执，聊天中断后可以接着执行 `/jobs show`。

本地集成分支 Windows/MSVC `fmt`、`check`、`clippy`、`test` 均以 0 退出，229 通过、0 失败、1 项付费在线测试忽略。此记录不改变 Baseline 8 原始验收计数。
