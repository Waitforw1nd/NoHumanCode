# NEXT-15 员工C首候选报告0.1

日期：2026-09-28。执行人：员工C / GPT-6-astra。执行ID：C-R16-01-20260928-E01。用户在当前员工C主对话正式派发，接续原400未执行任务；未另建任务/工作树/代理，未跨对话发消息。依据主仓契约v1.0及补充01、最新执行登记和本人第十六轮正式任务。

**状态：本人范围首候选已提交，静态检查完成，source/index clean/no-build冻结，等待经理整合真实B/D依赖。未编译、未运行组件测试；I01/I07/I08/I10均未动态通过。最终五门禁未放行、未执行。不自行APPROVED。**

## 1. 基线、候选和Git责任

| 项目 | 完整事实 |
|---|---|
| 接收基线 | 5194099ee4d1eb72af5f286826c72c6fd19d29e8 |
| 基线NoManCode tree | f48df8d01f64095ad32b1412ac5a4db45b849182 |
| 基线Rust tree | dfdedbfda40aba9502f1606cc466f2f32aa4e3d0 |
| 本人分支/工作树 | codex/c/host-setup-cli；根相对 ../nhc-c-host-setup-cli |
| 首候选HEAD | 1681233b222731397c9631178eadeed4fcdf1ad8 |
| 候选NoManCode tree | fd1d41de9e785a6d460a5eebbfd49c05c9291ee0 |
| 候选Rust tree | 83d938b89b07874a52508503919f77ea2c2fd1b8 |
| 源码提交人 | 员工C，仅本人独立树七个授权文件 |
| 主仓index、人员档案提交、跨树merge与整合人 | 仅项目经理；本人未操作 |

初始本人HEAD/branch/两tree/status/index实核完全匹配派发，无继承未提交改动。主仓后续管理提交不用于reset、merge或覆盖。候选提交成功exit0；2026-09-28T14:20:49.1057362Z原始回执记录status/index空，未发现命令行或执行路径属于本树的cargo/rustc/rustfmt/peachsh进程。本轮未启动构建/Host/CLI测试进程，不把别人的进程状态当本方证据。

## 2. 七文件实现及边界

全部产品路径以仓库根为基准，实际写入本人独立树；未改B/D源码、Cargo、锁文件、迁移或其它tests。

| 文件 | 本次实现 |
|---|---|
| NoManCode/rust-app/src/main.rs | 默认新库调用B的fresh_settings，显式--legacy-data才走原initial_settings，已有Settings优先；help说明check会创建/迁移/初始化而非只读。Host参数解析错误固定文案，防误粘贴秘密被clap回显。 |
| NoManCode/rust-app/src/server.rs | authenticated GET /api/host/config、PUT /api/host/routes/{id}、POST /api/host/config/lookup；StrictObject递归拒绝重复键，真实B请求DTO拒绝未知/类型；唯一合法Idempotency-Key、1MiB、Content-Type与路径错误；固定嵌套安全错误，无dispatch_state/Provider call。Host/Origin/site/token限制沿用，新GET也必须token。 |
| NoManCode/rust-app/src/cli.rs | host config get/operation、host route set；秘密模式三选一，无api-key值参数；调用D read_credential/into_string，复用B RouteInput.normalized和公开响应DTO。响应递归拒绝重复键，字段白名单、ID/revision/规范route/来源校验；检查本次secret/bootstrap token不得出现在响应，错误只输出固定文案。本方持有的秘密buffer尽力清零，明确不保证依赖/OS无副本。 |
| NoManCode/rust-app/tests/host_configuration_http.rs | 三个真实Router+Engine+fresh Store专项测试；配置通过HTTP写入，不预注入route。 |
| NoManCode/rust-app/tests/host_configuration_cli.rs | 六个真实CLI子进程专项测试；受控TCP仅用于恶意响应/断线验证，不作为真实Host或首次使用证据。 |
| NoManCode/rust-app/tests/host_initialization_workflow.rs | 两个真实binary首次使用测试，I10同一初始不存在DB全程用CLI创建路由/Project权限/选择/预算/授权及mockCompletion，SQL只读检查结果，无Store注入。 |
| NoManCode/docs/cli-first-release-acceptance.md | 安全默认启动、显式legacy、check写语义、三种秘密输入、原回执与当前配置、unknown诊断和I10最小链；明确完整阶段一与默认插件组合仍待后续统一验收。 |

PUT只有一次send。交给transport后断线/超时/重定向/超大体/截断/非法JSON/不可信身份与响应/stdout失败为operation_result_unknown、exit1、retryable=false；不自动lookup、重发、换key或调用Provider。先查原operation再查当前config；lookup404不证明原PUT永不提交。可信固定Host错误保留其分类；本地输入/bootstrap失败在PUT前明确未发送。

配置接口仅调用真实Engine应用接口，不直接写Store或自造事务/授权事实；无临时stub和永久假实现。旧Settings/account接口保留，新增guard错误形状仅用于/api/host/命名空间。最终旧功能回归尚未运行。

## 3. 验收映射：代码已备，动态全部待执行

| ID | 本方测试/断言入口 | 当前状态 |
|---|---|---|
| I01 | i01_safe_empty_start_explicit_legacy_and_existing_database_are_distinct：真实Host邻近合成legacy YAML，新库无隐式导入/secret/Provider；已有DB不覆盖；显式首次导入保持legacy来源；check真实创建DB及help语义。 | 未执行 |
| I07 | HTTP三个i07测试：三路径Host/Origin/site/token含GET，嵌套重复/未知/null/数字/保留ID/恶意URL/大小/类型/重复key拒绝，失败前后配置/secret/receipt无部分写；成功/回放/no-op/冲突/历史lookup与当前snapshot。 | 未执行 |
| I08 | CLI六个i08测试：human/JSON/stdout/stderr秘密检查，真实TCP收完整PUT再断线，Content-Length截断、超大体、重定向、非法/重复JSON、错误status/route/revision/ID/来源、额外字段、token或secret回显、stdout断管；每项只一PUT。无效stdin/参数无PUT，可信与恶意错误区分、no-op/原回放、unavailable与missing、lookup404在途不确定性。 | 未执行 |
| I10 | i10_same_empty_database_cli_configuration_permissions_budget_completion_and_restart：同一空DB真实Host+CLI；配置→Project权限/选择→预算→preview/Completion授权→单次mockCompletion与usage→显示名修改区分旧receipt/当前配置→真实kill+wait→同DB重启、新bootstrap与状态保全，Provider仍1。工具/写/Shell权限保持关闭。 | 未执行 |
| I11 | 最终固定组合check→test→fmt→clippy→wasm-check与旧功能回归；C唯一执行，任何失败停止，源码变化从check重起。 | 经理未放行，未启动 |

测试中的kill+wait及Provider计数目前只是待执行断言，不是已发生证据。I10不覆盖默认插件13项逐一装配/撤销，也不替代完整开发、审批、Diff/Checkpoint/恢复的新用户整链。没有付费live或release。

## 4. 真实依赖与下一操作

截至本候选，C树尚未合入B host_configuration/fresh_settings/Engine方法及D credential_input/lib导出，不能声称该树已可编译。本人只读核对B当时真实公开类型：HostConfigError.code/status/message、RouteInput.normalized、HostConfigSnapshot/RouteConfigResult及冻结应用签名；未复制B实现。D树已读到真实SecretInput.into_string及read_credential，所见HEAD为3675e3afeebb7c6ce73200a888588775bc55e3e9；此观察不替代经理review/冻结或依赖整合。

**当前明确回执：本候选source冻结，status/index clean，无本人构建；供经理普通merge真实依赖。** 经理合后本人第一操作实核新完整HEAD/两tree/status/index；再按真实组合做组件编译和I01/I07/I08/I10调试，问题交各唯一owner。最终五门禁必须另有固定组合放行，不能把本报告当放行单。源码若需继续改动先形成明确解冻/接续状态，不在经理整合中并行写。

## 5. 实际命令、失败与未执行

每次shell显式指定活动E盘工作目录。格式/检查TMP、TEMP、CARGO_TARGET_DIR均本人树.local（E盘）；只读使用已有工具链，没有安装/更新。没有cargo编译、组件运行、全workspace test、最终fmt门禁、clippy或wasm-check。以下仅六个授权Rust文件的局部格式检查，不能写成五门禁fmt通过。

Rust cwd为NoManCode/rust-app，六文件参数依次为src/main.rs、src/cli.rs、src/server.rs、tests/host_configuration_http.rs、tests/host_configuration_cli.rs、tests/host_initialization_workflow.rs。完整可复制原命令分行见证据command-index.json：

    rustfmt --edition 2024 --config style_edition=2024,skip_children=true src/main.rs src/cli.rs src/server.rs tests/host_configuration_http.rs tests/host_configuration_cli.rs tests/host_initialization_workflow.rs
    rustfmt --check --edition 2024 --config style_edition=2024,skip_children=true src/main.rs src/cli.rs src/server.rs tests/host_configuration_http.rs tests/host_configuration_cli.rs tests/host_initialization_workflow.rs
    git diff --check

最终限定检查使用rustfmt 1.9.0-stable (48a229ceae 2026-09-01)，2026-09-28T14:20:47.2975321Z至14:20:49.1057362Z，写格式0、check0、diff0、源码commit0；原日志真实为空。前一次同正确工具的局部检查亦0，日志另保留。设置RUSTUP_AUTO_INSTALL=0防止自动补装；未改仓库工具链配置。

保留的失败/工具问题：

- 初次在仓库根选中rustfmt1.7.1，局部格式化exit0，但不是项目实际工具链的验证。随后显式style_edition选项被该旧工具拒绝exit1，rustfmt-write2.log保留。读取现行环境记录后改从Rust cwd使用已安装1.9.0完成上述检查，不覆盖旧日志。
- 两次代理编排JavaScript语法错误，以及一次CLI补丁hunk顺序导致的verification failed，均未执行对应shell/写入，修正后才实际落盘；没有Rust测试退出码，不冒充测试失败或通过。
- 只读环境探测有shell exit1，来源是可选环境变量不存在；没有构建启动。证据复制及11对hash核验已完成后，末尾rg正则因传输转义报错，外层exit1；不影响已复制证据。后续用固定字符串查找复核，原失败不抹去。
- candidate-receipt.json原始commands数组受PowerShell字符串拼接影响合为一项；保留原件，用单独command-index.json逐项记录真实三个顺序命令，未改写原回执。

## 6. 原件、发布副本与SHA256

原件：根相对 ../nhc-c-host-setup-cli/.local/logs/next15-static/。发布副本：[NEXT-15证据/0.1](NEXT-15证据/0.1/manifest.json)。11份原件/发布副本逐字节SHA256相同，无路径改写；包含旧工具失败、两轮正确工具局部日志、原候选回执和命令补充索引。manifest同时记录七个工作文件SHA256。

manifest SHA256：F84A3B031459E419DA022B1D63A4E56C44C5177F9798147E11391F1733DFC458。

source、报告、静态检查、编译、动态验收、APPROVED、合main与发布是不同状态。本次仅完成首候选、静态检查和待整合冻结；无reset/clean/stash/push、跨树merge、旧树清理、D旧项目写入或真实密钥读取。
