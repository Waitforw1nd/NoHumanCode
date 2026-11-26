# 真实记事本「另存为」验收（显式 opt-in）

仅在**独占、可丢弃的 Windows VM / 测试登录桌面**运行，使用全新测试账户（无已开记事本、历史标签页、用户项目或凭据）。不要在正在使用的用户桌面运行。建议断开 VM 对外网络，保留回环。在同一桌面准备 Python 3.9+、含本次改动的 `coda.exe` 和系统记事本。无需模型账号、依赖包或管理员权限。

```powershell
# 从仓库根目录运行；将路径替换为本轮实际构建的二进制。
$coda = 'C:\build\coda.exe'
py -3 -B tests/manual/notepad_save_as.py --coda $coda --check-host-only
py -3 -B tests/manual/notepad_save_as.py --coda $coda --disposable-session
```

`--check-host-only` 只验证隔离数据目录、实际 CLI/Host、精确文件写范围、预算和回环路由授权，**不是** UI 验收。完整模式每次创建 `%TEMP%\coda-notepad-*`；只启动并登记自己的 Notepad PID。若系统版记事本向另一进程移交窗口，终止本次尝试，不要改用任务管理器挑选 PID。脚本不切焦点、不发键、不截图。把测试控制台和记事本并排摆放，便于看提示。

1. 确认新记事本显示 `workspace\original.txt` 的唯一标记。在该记事本手动选择「文件 → 另存为」，确认是标准保存框、编码为无 BOM UTF-8；不要输入文件名、按 Enter 或点击保存。回到测试控制台输入 `APPROVE`，然后点对话框的标题栏。
2. 回环模型先发 `computer_observe`，要求 `save_as_available=true` 且返回新的 `observation_ref`，再请求 `computer_save_as({observation_ref, path:"new-name.txt", expected_text:<标记>})`。看到控制台打印 `Pending` 和 `NOW click` 后，点回**测试控制台**并停留。脚本走 CLI 审批；必须收到明确的失焦/需重新观察结果，且新文件仍不存在。
3. 核对文件名框未被改写、对话框仍在。第二次在控制台输入 `APPROVE`，点回同一对话框标题栏并保持前台。脚本新发起一次观察，只批准本次 mock 提议且与预告路径和内容匹配的待审卡片，不要求你再切到控制台。不要手动保存。模型在下一轮发 `read_file`；匹配唯一文本、实际新文件字节一致、原文件字节与修改时间均未变且审批执行已完成，才会打印 `PASS`。

失败或超时即为未通过；保留控制台输出（含观察引用和审批状态）与打印的临时目录（含 `host.log`、独立 Host 数据和工作区），不要因出现 `#32770` 或新文件即判通过。脚本结束时仅终止其持有的子进程。取证后可直接销毁测试 VM。
