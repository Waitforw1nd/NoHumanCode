# NEXT-21A A0 attempt7

2026-11-01，员工 B。固定候选 `83b2e3a7a262ad31f0ca23dfaef1c55af10a536c`，产品树 `27a3c15b3e68aab3b9e7e7004a6da5d63bb6156e`、Rust 树 `8946b7e0d5536a78516e07fa858daeaa132970c9`。check、probe build、普通AC敏感性负控、S01均 exit0；probe SHA-256 `4a1c7b2a6ad8b5c32347745ecc127ad3b0043e7e830ceb90d0fc5f232a98a416`。攻防基础合并用例 exit101，之后 S05、S08、fmt check、Clippy 未执行。运行前后 185 源码文件清单一致，独立树 clean、结束无重型进程。原始现场在 `NoManCode/rust-app/.local/next21a-b-83b2e3a7-attempt7/`。

普通AC三 AccessCheck 均 API 成功，精确SID/AAP/无授权结果允许/允许/拒绝，保持挂起后封装记录进程已信号与Job归零；S01 LPAC 对应允许/拒绝/拒绝且实际正常通信。此结果不覆盖完整隔离和独立回收验收。

受限攻击probe结构化输出：七个模拟文件的读写均返回5，七个哨兵前后摘要相同；三项Host进程访问返回5，测试注册表读写返回5。WSAStartup返回10107，IPv4/IPv6/UDP操作未执行，测试在缺少tcp.ipv4结果时失败；10107不是10013拒绝证据。profile目录及注册表位置查询返回HRESULT `0x80070005`，未到创建动作；子进程创建返回1816而非当前断言要求5。上述分项记录不构成合并用例通过或完整边界验收，原件保留，等待经理裁决，未放宽断言。
