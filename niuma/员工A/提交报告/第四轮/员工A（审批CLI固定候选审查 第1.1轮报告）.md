# 员工A（审批 CLI 固定候选审查 第1.1轮报告）

## 审查身份与边界

- 任务：`A-NEXT03-04-AUDIT` 第二阶段 C 候选审查；审查时间：2026-09-26（Asia/Shanghai）。
- 执行者：员工 A；实际模型：`gpt-5.6-sol`。
- 固定基线：`e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`。
- 固定候选：`5bc5e0f4128b512009cb46e3014076f7d0069aeb`，分支 `codex/c/approval-cli`，审查时工作树干净。
- 候选相对基线只改授权三处：新增 `src/cli.rs`、修改 `src/main.rs`、新增 `tests/cli.rs`，共 1231 行新增、11 行删除；`git diff --check` 无输出。
- 本轮仅用 `git show`/`git diff` 静态审查生产代码及 CL01～CL10 证据。未改 C 源码、target、Git index；未重复运行 C 的定向测试或五门禁。C 已报告定向 9/9 与 check 通过，经理正在固定 SHA 上运行五门禁，该结果不是 A 独立测试证据。

## 结论

候选的大部分生产边界正确：CLI 分派发生在目录创建、instance lock、Store 和 settings 之前；服务模式默认值延后恢复；客户端固定 loopback，关闭代理和重定向，设置 2 秒连接/4 秒总超时并限制响应为 1 MiB；decision 先 bootstrap、POST 不重试；ID 经共享校验及 URL path segment 编码；成功响应经 typed DTO 重序列化，未知字段不会直接输出；人读模式不把 approved 宣称为已执行。

当前不能建议按此 SHA 验收。有两项生产阻断会绕过补充1的响应形状/token 要求，另有四类测试证据需补强。应由 C 修复并提交新固定 SHA 后做定向差异复核。

## 生产阻断

### [P1] 单卡命令可把列表形状当成功响应

位置：`src/cli.rs:157`、`src/cli.rs:332`。

`execute` 在命令分派完成后统一调用 `normalize_success(value)`；该函数不是按实际命令冻结期望类型，而是检查响应是否含 `approvals` 字段。于是 `approval get/approve/deny` 收到 HTTP 200 `{"approvals":[]}` 时会成功反序列化为 `ApprovalList`、exit 0 并输出列表。反过来，单卡响应只要恶意增加 `approvals` 字段，也会改变解析分支。这违反“列表与单卡按实际命令分别验证”和“不能把错误形状当成功”。

可操作修复：命令分派时携带 `ExpectedShape::List | Single`，list 只反序列化 `ApprovalList`，get/approve/deny 只反序列化 `ApprovalDto`；不能由响应内容选择类型。新增真实二进制 mock：三种单卡命令对 `{"approvals":[]}` 均 exit 1、stdout 空、固定安全错误；列表命令对单卡 DTO 同样拒绝。

### [P1] token 检查在 JSON 转义字符下可漏检

位置：`src/cli.rs:174`、`src/cli.rs:196`、`src/cli.rs:199`、`src/cli.rs:238`。

成功路径以 `value.to_string().contains(&bootstrap.token)` 搜索 token。若 bootstrap token 含可作为 HTTP header value、但 JSON 序列化必须转义的字符，例如双引号或反斜杠，decoded DTO 字符串可含原 token，而序列化后的 JSON 文本含反斜杠转义，literal `contains` 不再命中；随后白名单字段仍可能把 token 输出。错误路径只检查最后形成的 `Failure.message`，没有在 unknown-field 丢弃前递归检查整个 decoded ErrorBody，因此任意额外响应字段中的 token 不会被判为 unsafe response，虽当前不会输出，也未满足补充1“任意字段回显即拒绝”的明确要求。

可操作修复：在丢弃字段或重序列化前，对 decoded JSON 的所有 string value 做递归 literal token 检查；不要在 JSON 序列化文本上搜索。正常 ErrorBody 仍忠实传递，但任何字段含已知 token 时统一 fixed invalid response。新增含 `"`、`\\` 的 bootstrap token，分别放入成功 DTO 白名单字段、成功额外字段、错误 message 和错误额外字段；断言 exit 1、stdout 空、stdout/stderr 不含 decoded token、POST 恰好一次。

## 测试证据缺口

### [P2] CL01 没有证明两种资源各自的两种输出

位置：`tests/cli.rs:312`。

现有测试覆盖列表 JSON、单卡人读和空列表人读，但没有覆盖列表人读非空及单卡 JSON 精确白名单 DTO。契约要求“列表/单卡，人读与 JSON 精确”，且本候选恰好存在形状混淆，因此不是可忽略的排列缺口。

修订：至少增加单卡 JSON exact object 和非空列表人读；同时纳入上述反形状响应。

### [P2] CL04 多数 HTTP 错误只断言 code

位置：`tests/cli.rs:388`。

未知 approval 的 JSON ErrorBody 做了全对象精确断言，但未知 task 和重复决定只检查 `code`，没有证明 message/retryable/error、stderr-only、stdout 空和 exit 1 全部保持。补充1允许正规 ErrorBody 忠实传递，正需要端到端精确证据。

修订：对未知 task、重复 approve/deny 至少各断言完整 JSON、stdout 空、exit 1，并保留 resolved 事件一次的事实。

### [P2] CL03 的零本地状态路径断言可能检查错位置

位置：`tests/cli.rs:361`。

`local` 已是 tempfile 创建的目录，断言却检查 `local.path().join("../data-rust")`，目标落在该临时目录的父级而不是 cwd 内可唯一归属本次 CLI 的路径；并行环境若已有同名路径会误报，反之也没有直接列举 cwd 内零 DB/lock。测试连接的是内存 harness 服务，可证明无 provider 调用，但没有用实际运行服务持有 instance lock 来证明 CLI 不争锁。

修订：创建独立 temp parent/cwd，快照其内外预期路径，连接一个真实运行且持有 instance lock 的 Host；CLI 后证明 cwd/指定隔离根没有新增 data-rust、DB、lock，原 Host lock 未受影响，provider 计数仍为零。

### [P2] CL05 的非法 ID 用例先命中参数冲突，未证明 ID 分支

位置：`tests/cli.rs:433`。

当前 `..` 用例同时显式传 `--data-dir`。`main.rs` 的客户端分派先拒绝服务路径参数，再调用 `command.valid_ids()`，所以该用例 exit 2 并不能证明 dot/dotdot 被 ID 校验拒绝。测试也没有单独覆盖控制字符、129 字节拒绝、合法 128 字节及包含 slash/query/percent 的合法 ID 实际请求路径。

修订：每类 ID 使用无其他冲突参数的独立真实二进制调用，并用 mock 请求计数/捕获路径证明非法值联网前拒绝、合法特殊字符只占一个安全 path segment。保留敏感样式不回显断言。

## CL01～CL10 静态核对

| 条目 | 本候选静态证据 | 结论 |
| --- | --- | --- |
| CL01 | 真实二进制覆盖 list JSON、get human、empty human | 部分；见形状阻断与输出组合缺口 |
| CL02 | 真实 Host/runtime，approve 实际写文件并 finished；deny 不写且 resolved 一次 | 静态证据充分 |
| CL03 | 临时 cwd、无等待者 not_started、provider 计数 0 | 部分；相对路径断言及真实 instance lock 证据需加强 |
| CL04 | 404/409 与事件一次；仅首个 404 完整断言 ErrorBody | 部分 |
| CL05 | 参数冲突、port 0、敏感 ID；dotdot 用例与 data-dir 冲突叠加 | 部分；非法 ID 分支及合法编码路径需独立证明 |
| CL06 | bootstrap 缺字段/非法 JSON/过大、连接失败、decision 超时且 POST 一次 | 静态证据充分；新 SHA 应补错误转义 token |
| CL07 | no-proxy 环境、bootstrap/POST redirect 不跟随、第三端点计数 0、POST 不重试 | 静态证据充分；token 任意字段/转义字符仍阻断 |
| CL08 | 无 CLI 子命令时 check 与服务启动真实子进程，显式 port/path 参数保持 | 静态证据充分；原“默认”指服务模式分派，不强求零参数占用固定3090 |
| CL09 | unknown 忠实显示、help 无 token、已知字段 token 拒绝、额外字段丢弃 | 部分；token 扫描实现有绕过 |
| CL10 | C 声明定向 9/9、check 通过；经理五门禁在途 | A 未执行，等待经理固定日志 |

## 其他边界

- `GET /api/bootstrap` 仍是无认证 loopback 能力发放点；候选没有扩大为同机进程认证，符合补充裁决。
- `endpoint` 使用 URL path segment API，且 exact `.`/`..` 在联网前拒绝；包含 slash/query/percent 的合法共享 ID 被编码为一个 segment，CL04 用 `../bad?query=value` 证明未命中 decision 路由。
- 参数语法错误由 clap exit 2；敏感 ID 被通用“标识无效”替代，不回显输入。
- `ApprovalDto` 字段与已验收 HTTP DTO 一致，status/execution_state 做枚举检查，ID/preview/tool_name/decided_by 做既有安全校验；未知成功字段通过 typed 重序列化丢弃。
- 本轮未发现代理、重定向、自动 POST 重试、本地 Store 初始化或原 body/reqwest URL error-chain 直接输出路径。

## 下一步

C 应在新提交中只修上述两项生产逻辑及对应定向证据，并补强 CL01/03/04/05。A 收到新固定 SHA 后只审相对 `5bc5e0f4128b512009cb46e3014076f7d0069aeb` 的差异、`git diff --check` 和经理门禁结果，不重读无关代码。B 尚无固定候选，本报告不评价 B 在途实现。
