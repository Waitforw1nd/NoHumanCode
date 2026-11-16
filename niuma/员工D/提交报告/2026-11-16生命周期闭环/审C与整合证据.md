# C 测试与整合证据交叉审查

2026-11-16，D，只读。C 固定候选 `7f89a5c5210af8a864823145cb1f6191cad6ad94`，含 8 项公开生命周期/域竞争和 5 项独立组件回归。D 未运行任何真实测试。

| 项目 | 审查结论/状态 |
| --- | --- |
| Shell 后代/父进程退出/EOF/输出限长 | 有真实后代、显式 ack/release 屏障，目标对象由创建者 DuplicateHandle 转移；夹具只观察已知 parent/descendant 两对象，完整 Job 空及 reader 收束仍依赖 A 实现与组件证据 |
| 取消/超时/Host关闭/ordinary failure | 有取消到真实对象终态及下一合法命令正控；Git PATH 正控和同库第二 controller 拒绝，未凭总通过数代替条件 |
| unknown | 初稿 Drop 在未确认后释放 OwnedHandle 的问题已反馈。候选改为统一 NHC_TERMINAL_UNKNOWN、sticky 全局封门、同一进程停驻保留原对象；serial case gate 阻止同测试进程继续开新工作。不会因迟到清理把业务 unknown 改成成功 |
| 凭据夹具 | 初稿 `put_secret("route",...)` 不符合现行 route secret key；反馈后改 `route::route`，避免在目标执行前失败造成伪覆盖 |
| 证据边界 | component.rs 的模拟 DomainLease 只证明组件 seal/settle/容量，不证明真实 Store fence；候选已明确标注。13 项不能替代两个真实 Host、--check、schema迁移、崩溃恢复和旧epoch全矩阵 |
| 待后续固定 | C 已说明将修 verbatim Windows 路径对 Git alias 的影响，并为全部 async case 接 test_scope 和旧 Git 内嵌测试专用迁移；这些在本次 7f89 固定审查之后，不能提前宣称闭合或替 C 提交 |

结论：C 当前候选具备独立回归价值；路径/test_scope 跟进和实际编译运行仍待经理固定版本验证，不给完整生命周期/完整 U 的 APPROVED。原 unknown 与旧 B23/C35 不改判。

经理整合证据已独立复核：

- `component-check-2`（86e4f6be）和 `component-lib-build-1`（4ba4939a）的原日志 SHA256、记录候选双tree均相符，记录 exit0。它们是组件 check/build，不是全量门禁；详 `整合证据复核.json`。
- `component-matrix-1` 固定候选 `4ba4939a70211eb881ed5145f19a1595ef46def2`，产品 `fca1d86b3007197fad0eee75be28fa5b4c288154` / Rust `eac613098be2ceb583e158d53adfac52e980d856`：76 件证据哈希/长度全部相符，23 case 状态 passed 且 exit0，前后状态与源码文件哈希清单逐字相同，test/probe 当前二进制 hash 与 inputs 相符。详 `组件23矩阵证据复核.json`。这是 A 组件矩阵证据，未覆盖 B 整合、C 独立矩阵或 D CLI/HTTP。
- `component-check-1` 是经理处理绝对 git-common-dir 路径时的预检失败，记录 Cargo/gate 均未启动；保留历史。
- `integrated-check-1` 使用 D 工具首轮，MSVC 发现失败（exit1），`01-check.log` hash `1533d8424d35019b5fbb8412bf77b1c126684597f7fe603fa93dd8187667e578`；前后源码一致、gate_retained=true。经理确认 Cargo 未启动，需显式提供已安装 MSVC/SDK 根后新 attempt，不覆盖旧失败。

没有以 PID 列表、直接测试进程 exit0 或“当前看不到进程”推断进程树已清理。当前重型窗口全程由经理持有。
