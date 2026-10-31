# NEXT-21A A0 attempt6

2026-11-01，员工 B。固定候选 `bcd6d9b089da9fe922fd82d4fb5d42bd89cefa44`：check、probe build、普通AppContainer负控、S01 均 exit0，probe SHA-256 `e6df7ec22c9db3dd08bd94646c43b1f12bbd7065e07bffa091042e7cef41ef5c`。普通AC的三个AccessCheck均API成功，精确SID/AAP/无授权结果依次允许/允许/拒绝；挂起PID 55528，记录进程信号与Job active=0，未resume。S01 LPAC实际token对应允许/拒绝/拒绝，API均成功，PID 3672 正常通信退出。

S02/S03/S04/S07基础合并用例 exit101：LPAC probe PID 40832 resume后Rust标准库winsock初始化断言，返回10107导致probe退出101，无结构化攻击结果，不能当网络拒绝或实际越界证据；S05/S08/fmt/Clippy未执行。原始日志和现场在独立树 `NoManCode/rust-app/.local/next21a-b-bcd6d9b0-attempt6/`，无重型进程。普通AC profile删除及外部持句柄回收仍未验收。

普通probe夹具返工：显式记录 WSAStartup 原始返回，失败时不调用 std::net、不伪造网络请求结果，继续收集其他探测；所有原安全断言保留，10107不算10013拒绝。补充必需WinSock feature；待新候选新attempt验证。
