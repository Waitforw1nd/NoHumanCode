# C-R4-01 / NEXT-04A：审批 CLI 薄客户端

2026-09-26，经理 GPT-6 派发；C 实际模型 gpt-5.6-sol。本轮并行 B Workspace 安全实现，无源码交叉；CLI 首片仅接已验收审批 HTTP，不宣传完整开发者闭环。

## 基线与唯一范围

完整基线 `e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`；工作树 `../nhc-c-cli/`，分支 `codex/c/approval-cli`。唯一源码三处：`NoManCode/rust-app/src/main.rs`、新增 `src/cli.rs`、新增 `tests/cli.rs`。在 main 中声明 cli 模块，不动 lib/Cargo/server/Engine/Store/UI。C 独立分支提交本人源码；经理 review 与串行整合，共享 index 仅经理。报告/身份仅共享 `niuma/员工C/`，新增 `提交报告/第四轮/员工C（审批CLI 第1.0轮报告）.md`，存在递增；登记实际接收时刻/执行ID/继承修改。保留旧工作树和档案，不清理覆盖。

## CLI 行为

- `peachsh [--port PORT] [--json] task approvals TASK_ID`
- `peachsh [--port PORT] [--json] approval get APPROVAL_ID`
- `peachsh [--port PORT] [--json] approval approve APPROVAL_ID`
- `peachsh [--port PORT] [--json] approval deny APPROVAL_ID`

只连接 `http://127.0.0.1:PORT`，默认3090；本轮不接受任意base URL。reqwest关闭代理和重定向，设置有限请求超时。写操作先GET `/api/bootstrap` 得token，再POST已验收decision；token仅内存不打印，不持久、不进入错误链输出。用URL路径段编码ID，不能字符串拼接形成路径/query注入；调用既有标识校验或等价只接收有效ID。不要从非2xx/缺字段bootstrap继续决定请求。

CLI分派必须先于任何数据目录创建、instance.lock、Store open/迁移和settings初始化；不直接打开Store、不启动Engine/provider、无自动resume。无子命令原启动服务和参数保持，`--check`原行为保持，`--check`加子命令拒绝；`--json`无子命令拒绝。数据目录等服务参数不得让客户端初始化本机状态。approve/deny失败不重试POST。

成功默认可读输出审批id/状态/执行状态；`--json` stdout仅有效JSON，列表保持服务器包装，单卡保持DTO；不能根据approved伪称执行成功。失败非零退出码，结构化HTTP ErrorBody保留code/message/retryable/error（error=message）；--json失败只stderr有效JSON，stdout空；传输、非法响应、超时固定安全本地错误，不输出token、原body、服务器任意HTML或reqwest完整URL错误链。解析错误exit2，运行/HTTP失败exit1，成功exit0。HTTP正规错误固定文案沿服务端，不靠解析中文猜分类。

不接旧changes/restore，不另造业务规则、不新增重试/resume/创建任务/发消息/SSE子命令；后续Workspace接口冻结后单独扩展。

## CL01～CL10

| 编号 | 必须证据 |
| --- | --- |
| CL01 | 真实Host+真实CLI子进程，列表/单卡，人读与JSON精确，空列表 |
| CL02 | 真实pending批准/拒绝，Host持久决定/resolved一次及工具副作用；不把批准响应等同执行成功 |
| CL03 | 指向运行Host时CLI不抢实例锁、不建本机data-dir/DB，不调用本地provider；无等待者批准只决定不执行 |
| CL04 | 未知审批/任务404、重复决定409，ErrorBody准确、stderr JSON/stdout空、退出码1 |
| CL05 | 非法参数/ID、路径query注入、check冲突/json无子命令，退出码2且零文件副作用 |
| CL06 | 未运行Host/超时/无效JSON/缺字段，安全错误不泄token或原响应；不自动POST重试 |
| CL07 | mock bootstrap失败、重定向、恶意proxy环境、POST重定向：token不送第三端点，计数证明 |
| CL08 | 默认无子命令启动服务与原check/port/data-dir/legacy/workspace兼容；临时目录验证 |
| CL09 | stdout/stderr/help/错误中无bootstrap token与敏感body；approved+unknown忠实显示 |
| CL10 | check/test/fmt/clippy/wasm-check五标准门禁及定向cli；真实退出码、未跑/环境失败如实 |

测试仅本机mock和临时数据，cli真实二进制执行，不只调用函数。标准 build.ps1，独立target，MSVC用现有环境，不改脚本，不发布build/付费live。固定完整候选SHA，报告逐项CL证据、实际语法/输出/退出码、门禁日志与限制；更新共享身份。经理与A固定候选审查，提交不等于验收/合并。
