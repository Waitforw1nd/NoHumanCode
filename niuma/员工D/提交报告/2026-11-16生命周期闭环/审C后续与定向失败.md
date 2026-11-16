# C 后续候选与历史定向证据复核

2026-11-16，D，只读。承接已提交 `审C与整合证据.md`，本次静审固定 C `fc0bdc04ef43dcc319ad55a4340f94dde190be78`、`65d32c36cea7d8ec077603a8d76464530d0bd55a` 和 `69226513d3aaaa4d48ea1dc76d96698c501a18a3`；没有启动测试进程或改C文件。

| 变化 | 本次判断 |
| --- | --- |
| 原15个Git契约测试迁入内部专属文件 | 13个async保留断言并包真实test_scope；保留真实Store/Engine授权，不是成功桩。内部目标私有访问由B注册，外部git_diff目标保留无权拒绝与Engine真实patch正控 |
| 超限/超时反假绿 | 使用B一次性受控spec注入；要求实际创建identity/原process handle、Job空及双reader EOF/join。overflow-started证明目标真正执行，不让启动前Unavailable冒充超限覆盖 |
| 嵌套环境测试 | 改为隐藏原Child与文件stdout/stderr，30秒界到达后原观察者停驻、保留Child；exact选择器另验running 1 test与1 passed，避免0匹配。此为保守留持，不以父方deadline杀未知子监督者 |
| Windows Git alias路径 | 仅夹具去verbatim前缀，避免Git sh解释成网络路径，不放宽产品路径授权 |
| EOF夹具修正 | 后代创建前清父stdout/stderr继承位并读回留证；仍保留父已退出、双EOF/join、后代原对象仍live的同时断言，没有放宽观察条件。继承泄漏是静态假设，动态是否修复由新窗口证明 |
| Git PATH正控修正 | 保留原PWD/Join-Path命令并增加空格目录；新增setup唯一工具结果、JSON可解析及实际exit0。依赖B bc291abe的cwd修正；未绕开生产入口或将Finished等同命令成功 |

这些补丁未显示弱化原安全断言的新增问题。模拟FixtureLease仍只证明夹具/组件持权，不代表真实数据域；Shell调用Git alias只算Shell树，不能替代原生Git整项票。完整U/schema18、两个真实Host、崩溃恢复与旧epoch仍不得从本组通过数推导。

历史整合证据另以原字节复核，详 `整合定向证据复核.json`：逐件SHA256/长度、manifest步骤和result一致性、前后源码、候选Git双tree。integrated-check-2 的check返回101（编译错误），不是通过；六件原证据hash相符，目录额外出现经理归档的gate-archived.lock/manager-closure.json，所以严格artifact set比较不相等，这不属于原清单中的hash篡改。integrated-targeted-2在第三步返回101，14件原证据相符，原日志16 passed/2 failed，失败分别是EOF观察和Git diff正控。后两个target当时未执行。这些失败保持，不因C后续改动或新full窗口而改判。

早期manifest没有runner_exit_code字段时记录字段缺失，不根据step0或status伪造外层退出码。本次审查脚本初稿在该缺失字段触发KeyError、未生成报告，修为明确记录null后完成复核；不是产品测试失败或重试到绿。当前 integrated-full-1 尚在经理窗口，最终证据需在完成后另做独立复核，不把增长中的日志当作固定证据。
