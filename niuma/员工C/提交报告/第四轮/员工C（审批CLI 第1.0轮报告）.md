# 员工C：审批CLI第1.0轮报告

日期：2026-09-26；任务 `C-R4-01 / NEXT-04A`；执行标识 `C-R4-01-20260926-024836`；实际模型 `gpt-5.6-sol`；实际接收时间 `2026-09-26T02:48:36+08:00`。

开发基线 `e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`，工作树 `../nhc-c-cli/`，分支 `codex/c/approval-cli`。初始提交 `5bc5e0f4128b512009cb46e3014076f7d0069aeb`；经理 review 1.0 后的生产修复提交 `fd3e7a373466e4b7628196faaf0974e66b461559`；最终固定候选 `0d4dc9af863458e3d6fe2ec7c8b380eebaaafdcc`。最后一提交只在测试中增加 instance-lock 固定错误断言，生产输入与 `fd3e7a3` 相同。

候选相对基线仅新增 `NoManCode/rust-app/src/cli.rs`、修改 `src/main.rs`、新增 `tests/cli.rs`，共新增 1469 行、删除 11 行；`git diff --check` 无输出。源码 SHA-256：`cli.rs` 为 `D8D6DABBC32D4EA2CDE97EF6709775C027241BEA0465311DE0833C872AB89929`，`main.rs` 为 `06C279B718C399548F649D8FAEABD65D935DC59EE9E102E9925AB210987A6D54`，`tests/cli.rs` 为 `2634730A2DC69BAD993CF50D83AE1E000964B1906256EBC18A78EEC41A67DBAA`。

## 交付行为

- 新增 `task approvals TASK_ID` 与 `approval get/approve/deny APPROVAL_ID`，支持全局 `--port` 和 `--json`。默认连接固定 loopback `http://127.0.0.1:3090`；关闭代理和重定向，连接超时 2 秒、总超时 4 秒，响应体上限 1 MiB。
- CLI 分派先于目录创建、instance lock、Store/settings/Engine/provider 初始化。没有子命令时原服务启动保持，`--check` 保持；客户端与显式服务路径参数、`--check` 冲突，语法错误退出 2。
- ID 复用 `validate_persisted_id`，另拒绝精确 `.`/`..`；合法 slash/query/hash/percent 作为一个 URL path segment 编码。port 0、控制字符、129 字节、敏感样式均在联网前拒绝，合法 128 字节保留。
- approve/deny 先读取 bootstrap token，仅存内存，再执行一次 POST，不自动重试。决定请求发生网络失败或超时时输出 `decision_result_unknown` 与“审批决定结果未知，请用 approval get 查询”；bootstrap/读取失败用安全本地错误。
- 列表命令固定反序列化完整 `ApprovalList`，get/approve/deny 固定反序列化完整 `ApprovalDto`，不由响应内容猜形状。成功响应只输出白名单 DTO；未知字段丢弃，可识别凭据字段拒绝。
- 决定响应在丢弃未知字段前递归扫描解码 JSON 的对象键和字符串值；任意字段含本次已知 bootstrap token 均固定拒绝。此保护准确限定于已知 token 与可识别凭据，不声称识别任意未知秘密。
- 人读成功输出 `<id>\t<status>\t<execution_state>`；空列表输出“没有审批请求”。approved 与执行状态分别显示，不把批准宣称为执行完成。正规 HTTP ErrorBody 保留 `code/message/retryable/error`；JSON 失败只写 stderr，stdout 空，运行失败退出 1。

## CL01-CL10 证据

| 验收项 | 最终证据 |
| --- | --- |
| CL01 | 真实 Host 与真实 CLI 子进程覆盖非空/空列表、单卡的人读与 JSON；JSON 单卡精确等于 Host DTO。四命令分别拒绝反向响应形状，exit 1、stdout 空；两个决定 POST 恰两次。 |
| CL02 | 真实 pending approve 后 write_file 只执行一次并 finished；deny 不写文件；`approval.resolved` 各恰一次。响应分别显示审批状态与执行状态。 |
| CL03 | 隔离 cwd 下远程 CLI 不创建默认 data-rust/DB/lock、不调用本地 provider；无等待者批准只持久决定、不执行。 |
| CL04 | 未知审批、未知任务 404 与重复决定 409 均精确保持完整 ErrorBody、exit 1、stdout 空；恶意路径不命中决定路由，resolved 不增长。 |
| CL05 | dot/dotdot、控制字符、129 字节、敏感样式、port 0、check/json/服务参数冲突均 exit 2 且无文件副作用；合法 128 字节及 slash/query/hash/percent 的捕获 URI 证明单路径段编码。 |
| CL06 | 未运行 Host、连接/决定超时、bootstrap 缺字段或无效/超大 JSON、审批无效响应均固定安全错误；决定 POST 不重试，结果未知文案不假称失败。 |
| CL07 | bootstrap 和 POST 重定向均不跟随；恶意 proxy 环境被忽略；第三端点零请求。token 回显固定拒绝，POST 计数证明无重试。 |
| CL08 | 原 `--check` 与显式 port/data-dir/legacy/workspace 服务启动兼容；真实 Host 持有 instance lock 时第二服务进程 exit 1 且 stderr 命中既有锁错误，CLI 列表仍成功。 |
| CL09 | approved/unknown 忠实显示；help、stdout、stderr 和错误均不泄 token。含双引号/反斜杠 token 在成功白名单字段、嵌套额外字段、错误额外键和值中均被递归识别并拒绝。 |
| CL10 | 五项标准门禁、全量测试和真实二进制定向测试均有独立日志与起止/退出码元数据；首次 MSVC 环境启动失败也原样保留。 |

## 门禁与版本对应

完整五门禁在生产修复候选 `fd3e7a373466e4b7628196faaf0974e66b461559` 上执行：

| 命令 | 时间（+08:00） | 结果 |
| --- | --- | --- |
| `build.ps1 -Action check` | 03:16:37–03:16:38 | exit 0 |
| `build.ps1 -Action test` | 03:16:38–03:17:02 | exit 0；233 passed、0 failed、1 个付费 live ignored |
| `build.ps1 -Action fmt` | 03:17:02–03:17:03 | exit 0；零字节输出日志保留 |
| `build.ps1 -Action clippy` | 03:17:03–03:17:04 | exit 0 |
| `build.ps1 -Action wasm-check` | 03:17:04–03:17:05 | exit 0 |
| `cargo test -p peachsh --locked --test cli -- --nocapture` | 03:17:05–03:17:14 | exit 0；11/11 passed |

最终候选 `0d4dc9af863458e3d6fe2ec7c8b380eebaaafdcc` 相对上述 SHA 仅增加 CL08 的 5 行测试断言；生产文件哈希不变。该 SHA 上 `fmt` exit 0、`clippy` exit 0，初始化 MSVC 后的定向 CLI 11/11、exit 0。首次直接 cargo 因当前进程找不到 `cl.exe` 于 03:18:44 exit 101，失败日志没有覆盖；随后通过项目脚本初始化同一会话并重跑成功。按经理指示，未对不受纯测试断言影响的 check/wasm 机械重跑。

原始日志和元数据位于 [C-R4-01证据](C-R4-01证据/)，文件名携带对应短 SHA。证据保留开发机实际绝对路径作为当次原始日志事实；本报告正文及链接使用仓库相对路径。未执行付费 live、release、推送或发布。

## 复审闭合与限制

经理 review 1.0 的两项阻断已闭合：F1 改为由命令固定 List/Single 类型；F2 改为递归扫描解码 JSON 的对象键和字符串值，覆盖需 JSON 转义的 token。A 对 `fd3e7a3` 的差异复核确认 F1/F2 以及 CL01/03/04/05 补证闭合、无新增生产阻断；其后按建议在 `0d4dc9a` 收紧 CL08 锁错误证据。

CLI 使用 bootstrap token 是复用既有浏览器跨站防护，不构成同机进程认证。决定成功只代表审批决定已被 Host 接收；是否执行以及最终结果必须以 `execution_state` 和后续查询为准。当前候选等待经理最终 review 与串行整合，员工不宣告验收、合并或发布。

## 作者补充：证据路径归档

2026-09-26 按项目路径可移植性要求完成证据相对化。处理前先将原始 30 个日志/元数据文件按原字节复制到仓库本地 `.local/review-evidence/C-R4-01-0d4dc9a/`，逐文件 SHA-256 与共享证据处理前清单一致；原始清单文件 `original-before.sha256` 自身哈希为 `7FE5A5A8BC59097F6963129B5D5217BEEAFF8A249688B47F7E7C1452F8D00AF0`。

共享 [C-R4-01证据](C-R4-01证据/) 中仅把开发机源码工作树、独立 target 和用户 Cargo 缓存路径分别表示为 `<source-worktree>/NoManCode/rust-app`、`<cargo-target>`、`<cargo-cache>`。原始时间、命令、退出码、测试统计与零字节 fmt 日志未改。前后逐文件哈希见 `path-normalization-before.sha256`、`path-normalization-after.sha256`，映射和边界见 [路径相对化说明](C-R4-01证据/路径相对化说明.md)。这次归档没有重跑产品、修改源码或操作共享 Git index。
