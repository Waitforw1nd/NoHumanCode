# NEXT-21A A0 基础静态候选

2026-11-01，员工 B。依据 [A任务](../../../项目经理/任务/2026-11-01NEXT-21A-B进程隔离先决实现.md)、[补充01](../../../项目经理/任务/2026-11-01NEXT-21A补充01基础可行性验证顺序.md) 和 [设计审查](../../审查记录/第二十轮/2026-11-01NEXT-21设计审查与A切片裁决.md)，先交四项基础路径的静态版本；A未完成，安全隔离未经验证。

- 独立树 `../nhc-b-external-runtime-isolation`，分支 `codex/b/external-runtime-isolation`，基线 `69ccc53ece09e1ad04edd02521ef74f1b5e65aa1`。
- 本候选 HEAD `0fe116ab95235fbacc62dd818f12a81504ae5807`，产品树 `8e7cffd8e21c09cf7150c02257ae42dae5436ac4`，Rust 树 `cd4d4256f49ade1135b88810bac51b4491c36865`。status/index/unmerged 空；仅六项授权产品文件。
- 仅执行 `cargo fmt --all` 和 `git diff --check`，未编译、构建、运行 Host 或测试；格式不是编译通过或安全通过。首次动态需经理本次静态审查并放行。

## 候选内容

`src/external_process_sandbox.rs` 是未接入现有 Host 的试验模块：由调用者提供期望 SHA-256；复制到新建目录，对源与副本文件句柄校验并限制路径组件 reparse、文件硬链接和共享删除/写；不声称完成已认证安装或供应链。创建本次 AppContainer profile、设置试验目录 ACL，零 capability，LPAC 属性、原子 Job-list、句柄白名单及 suspended 启动；核 token 的 AppContainer/LPAC/SID、capability 计数、完整性、Job 和经进程映像查询的文件身份后 resume。Job 限进程数1、内存128 MiB、CPU 3秒；输入/输出限额，取消或超时进入 250ms grace 及同一2秒 cleanup deadline，验证进程退出和 Job 活跃数归零，失败报告 `cleanup_unknown`。各阶段记录原始码与 profile/进程事件；并不将运行成功等同权限边界成立。

`examples/external_process_probe.rs` 只操作临时 fixture 固定文件名、受控 listener、自身 AppContainer profile/注册表及其监督进程的访问权限；文件内容不输出。各权限结果保留 raw OS code；profile 文件创建、写入与删除分别记录，删除失败不掩盖已成功创建。profile 注册表键创建和清理分别记录。示例外网明确 `not_tested`，不把连接失败当作拒绝证明。

`tests/external_process_isolation.rs` 注册四个基础测试，读取外部 runner 指定的 `NHC_ISOLATION_PROBE` 和 `NHC_ISOLATION_PROBE_SHA256` 并核实实际二进制；人工指定路径与环境不得被误认为产物来源证据。临时 fixture 使用 keep 保留，记录原始 stdout/stderr、事件、代码和已执行路径的哨兵摘要。`tests/support/external_process_isolation/mod.rs` 的普通进程负控以 Job-list 创建时归属独立 Job、最多2进程、父退出 kill-on-close；受控负控只尝试自建哨兵。Cargo.toml 仅添加 windows-sys 0.59 的 Windows feature，未升级依赖；lib.rs 仅注册模块。

## 当前覆盖和缺口

| 项 | 当前候选有源码的子项；全部未执行 |
| --- | --- |
| S01 | LPAC 启动、token/Job、stdio 和退出的正向用例。profile 创建/删除的 HRESULT 与名称记录；强杀后 profile 实际残留仍待查。 |
| S02 | 自建 DB/WAL/Key、工作区、另一项目、用户资料、公有 ACL 哨兵读写；AppContainer 真实 profile 文件创建/写/清理与 profile 注册表创建/清理结果。自建注册表哨兵尚未完成设置和断言；公共 ACL 的实际可达/拒绝尚未知。 |
| S03 | 分别尝试 Host VM_READ、VM_WRITE/VM_OPERATION、DUP_HANDLE 和派生子进程，普通进程负控和受限结果分别记录。未白名单的独立哨兵句柄、breakaway及完整跨进程攻击待补。 |
| S04 | 受控 IPv4/IPv6 TCP loopback 及 UDP loopback，普通进程需要连接成功；受限只有明确10013才判策略拒绝。外网和 DNS 未测试，尚不能判 S04 通过。 |
| S05 | 挂死超时和启动后取消；真实 Host 强杀各交界、CPU/内存耗尽、管道阻塞及独立进程句柄观察待补。probe 的 `cpu`/`memory` 模式有代码，尚无对应用例和证据。 |
| S06 | 非 Windows 返回 unavailable 有代码；尚无平台编译、注入故障或不降级的独立验证。 |
| S07 | Job 约束的普通进程与 LPAC 对同一自建哨兵/监听器进行部分负控；未执行，不能以控制进程启动失败证明隔离。 |
| S08 | 错摘要拒绝且无 child 创建的用例；重解析、替换竞态、完整依赖/系统 loader 清单及 profile 残留尚未测。 |

静态风险需经理审：预备阶段同步 profile/ACL、源读取、哈希及 `CreateProcess` 等系统调用不能由当前线程强制打断，单个调用卡住时5秒不是已证明的硬墙钟界限；本候选只在相应调用返回时检查时间。若要覆盖 Host 强杀与不可中断初始化，仍须独立外层监督/实际进程句柄证据，不能凭此代码宣称已满足完整S05。测试里目前也未建立专用注册表哨兵，因此该探针结果若为找不到路径必须归入夹具/未验证而非权限拒绝。当前 `CreateAppContainerProfile` 还可能创建用户 profile 资源；普通返回时报告删除 HRESULT，但 Host 被杀后 profile 残留未核，不能声称无副作用。

## 建议 A0 窗口

经理审后，以新 attempt 记录前后候选/源码清单、工具链、各命令 stdout/stderr/UTC/exit、fixture 路径及 `external_process_probe.exe` SHA-256：

1. `cargo check --offline --locked --workspace --all-targets`。
2. `cargo build --offline --locked --example external_process_probe`；从本次指定 target 的 `debug/examples/external_process_probe.exe` 计算 SHA，设置上述两个变量到该精确路径和摘要。
3. 分别运行 `cargo test --offline --locked --test external_process_isolation s01_actual_lpac_process_exchanges_stdio_and_exits -- --nocapture`、`s02_s03_s04_s07_compare_real_sentinels_with_ordinary_control`、`s05_timeout_and_cancel_are_not_successful_execution`、`s08_wrong_digest_cannot_launch`，逐项首失败停止，保留 fixture/日志和实际拒绝码。
4. 前述全通过才运行 `cargo fmt --all -- --check` 与 `cargo clippy --offline --locked --workspace --all-targets -- -D warnings`。

单项测试中缺 IPv6、ACL 创建、启动、超时或无法枚举 profile 均属夹具/环境/执行失败，不能写成权限拒绝；任何 LPAC 成功读写受限哨兵、成功网络/进程能力属于边界失败。即使四项通过仍不是A完整验收，更不代表NEXT-21或process可用。

B 交此静态候选并等待经理首次动态前审查；尚未释放后续 A 实施职责。人员报告/身份在主仓，独立树提交仅含授权产品文件，由经理负责主仓暂存及整合。
