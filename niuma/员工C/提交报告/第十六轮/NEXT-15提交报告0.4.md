# NEXT-15 员工C报告0.4：旧HTML/JS设置HTTP级兼容补证

日期：2026-09-28；员工C / GPT-6-astra；执行ID C-R16-01-20260928-E01。经理在当前对话明确续办：先归档0.3，再在C原唯一范围补真实Host binary旧HTML/JS入口、实际旧settings读取/更新与新revision/receipt联动、秘密不新增回显、Provider零调用和既有认证边界。0.3和attempt1原始失败已封存保留；没有新派任务、代理或跨对话发送。

**当前状态：限定编译及12项组件全部通过；I11旧网页HTTP兼容子项已补证，不等于完整I11通过。当前候选source/index clean/no-build持续冻结。最终固定组合check/test/fmt/clippy/wasm-check仍未放行、未执行；本报告不APPROVED，不宣称阶段一完成。**

## 1. 候选、范围与Git责任

本人源码树：仓库根相对 ../nhc-c-host-setup-cli；分支codex/c/host-setup-cli。本人只提交独立树授权源码，人员资料只活动主仓本人目录。主仓index、档案提交和跨树merge仍仅经理，本人未操作。

| 节点 | 完整HEAD | NoManCode tree | Rust tree |
|---|---|---|---|
| 本次续办接收，0.3已封存 | d43db398244850d69de96c3e3c616b428f872d39 | 02cb4fb0533639970fd27bba93fea50a43895c43 | 63a2b26952faa94b8b60f3bcb0eb92c92a730532 |
| 首次兼容测试候选，编译失败保留 | 664f5a9194781a2f1a6f089da1b6e85afd4703c4 | 3e8b44c89372c860b3e3f86d475854ac2d2549c0 | a32d0f6505786a09567af869a9cc5ba77d6ab5eb |
| 修复后实际受测候选 | 57f750b3ddcd2c00314ca220f3fb632196970fe6 | 957940003aa88443765da43150c59bca80f5b812 | 040de91e08c3bfd6cc3ee3c89ff1755bd121408b |
| 当前交付，受测后仅更新说明 | dd2a66c9bfdde0a85f0831a7bcf8d1f7bc38e521 | 7a3c0c8ea88eea4b830701be607cc8c149c80a3f | 040de91e08c3bfd6cc3ee3c89ff1755bd121408b |

本次相对d43db39仅修改两个原授权文件：NoManCode/rust-app/tests/host_initialization_workflow.rs新增一个I11兼容测试及HTTP辅助；NoManCode/docs/cli-first-release-acceptance.md同步组件结果和验收边界。没有修改main/cli/server产品实现、B/D源码、旧web产品文件、公共DTO、Store/Engine/lib/Cargo或其它测试；无新依赖或浏览器框架。

提交dd2a66c与受测57f750b的Rust tree完全一致，git diff --exit-code 57f750b3ddcd2c00314ca220f3fb632196970fe6 HEAD -- NoManCode/rust-app真实exit0。动态结果只绑定受测候选；文档提交没有被写成再次运行测试。

## 2. I11兼容组件实际链路

测试入口：NoManCode/rust-app/tests/host_initialization_workflow.rs中的i11_real_host_legacy_web_settings_roundtrip_authentication_and_new_receipts。使用同文件真实Host子进程辅助、初始不存在的web-state数据库和本地计数mock；没有Store预注入、没有legacy YAML导入代替旧UI流程，SQLite只读核验。

实际步骤与断言：

1. 启动真实peachsh binary，bootstrap获取本次Host令牌。HTTP加载/、/app.js、/style.css，核状态200、真实Content-Type、no-store、实际返回内容与嵌入资产一致及已知秘密不出现。资源下载不是整个证据；后续实际执行与app.js一致的API请求和持久化检查。
2. 模拟app.js的GET /api/settings → PUT /api/settings {settings, keys} → GET读回：从空routes读取实际Settings，使用旧入口创建指向本地mock的路由并保存测试凭据。读回完整公开Settings匹配，keys仅为{local:true}可用性布尔值，newapi_key=false，无凭据值/token回显；新Host config revision由0到1。
3. 用真实新CLI --keep-credential修改显示名，成功receipt revision=2。旧GET立即看到新显示名；按旧JS的留空密码行为发送keys={}再更名，保存凭据仍可用，新Host revision到3。
4. 保持原新CLI operation回执revision=2及原route，当前GET为revision=3的新显示名；发送expected_revision=2的新写返回409/host_config_revision_conflict，配置不变。只读核host_config_requests恰有1条，旧写和失败CAS没有伪造新的operation receipt。
5. 全链显式核Project列表空、network_authorizations/network_calls/budget_calls为0；mock Completion与Models计数均0，没有由配置自动授权或调用Provider。
6. 真Host kill+wait后对同一DB重启，bootstrap token改变；旧Settings和Key可用性、新当前配置及原receipt均保持；再次核权限/调用表空和Provider=0，然后kill+wait退出。

既有认证语义没有被重定义：同源旧settings GET在没有写令牌时仍可读；PUT必须具有正确bootstrap token。分别实际请求缺token、错token、错Host、异Origin、cross-site写入均403；错Host/Origin/site读取也403，拒绝前后仍空设置/revision0。新/api/host/config GET无token仍403。该差异是既有兼容契约，未把旧GET包装成新authenticated GET。

所有正常和拒绝HTTP响应由统一辅助检查已知测试凭据及当前bootstrap token不出现；真正bootstrap响应仅用于取得令牌，不把其必要返回误当泄露。CLI输出另沿用原秘密断言。没有声称能识别任意未知秘密元数据。

**这是HTTP级兼容验收：真实binary提供原HTML/JS/CSS，测试模拟已读取app.js的真实JSON交互，验证实际保存/读取/认证/版本/重启。没有启动浏览器、执行JS/DOM、点击按钮或验证布局，不宣称浏览器交互验收。** 共享Host辅助日志仍带历史I01/I10前缀，I11也调用该辅助；三项并行启动日志交错，不能只凭该前缀归属PID。I11专属阶段日志、源码真实kill+wait及3项完整通过共同构成证据，不伪造单独浏览器运行。

## 3. 原始命令、UTC和退出码

每次shell显式E目录。驱动从本人树根运行、内部从自身目录定位NoManCode/rust-app，命令未改变：

    pwsh -NoProfile -File .local/next15-component.ps1 -Attempt attempt3-fallback
    pwsh -NoProfile -File .local/next15-component.ps1 -Attempt attempt4-fallback

| 顺序及原命令 | attempt3-fallback / 664f5a9 | attempt4-fallback / 57f750b |
|---|---|---|
| cargo check -p peachsh --bin peachsh --locked --offline | 15:21:32.6313753Z–15:21:33.2174884Z；exit0 | 15:22:21.8738676Z–15:22:22.2485383Z；exit0 |
| cargo test --locked --offline --test host_configuration_http -- --nocapture | 15:21:33.5381803Z–15:21:34.1461921Z；3/0/0，exit0 | 15:22:22.5226310Z–15:22:23.0606273Z；3/0/0，exit0 |
| cargo test --locked --offline --test host_configuration_cli -- --nocapture | 15:21:34.5294103Z–15:21:36.7583946Z；6/0/0，exit0 | 15:22:23.3577902Z–15:22:25.5425313Z；6/0/0，exit0 |
| cargo test --locked --offline --test host_initialization_workflow -- --nocapture | 15:21:37.0679217Z–15:21:37.7684379Z；测试目标编译失败，exit101，立即停止 | 15:22:25.9264750Z–15:22:32.4803950Z；3/0/0，exit0 |

日期均2026-09-28、时间UTC，三元为passed/failed/ignored。attempt3外层pwsh exit1；workflow因E0432未编译成功，三个workflow测试没有运行，不能记为3个断言失败或通过。attempt4外层exit0，共12 passed / 0 failed / 0 ignored，没有过滤或ignored。I01/I10本轮也真实重跑通过，未借0.3旧证据充本候选结果。

编译根因：C新增测试使用use reqwest::Method::{GET, PUT}，但Method是struct不是module。仅把该写法改为本地关联常量引用；未改产品行为或弱化断言。失败workflow.log原件保留。修复后固定57f750b，从组件check起按原顺序完整重跑。

两候选形成时均使用Rust目录现有rustfmt进行单文件格式和check，各exit0，随后git diff --check0。原命令：

    rustfmt --edition 2024 --config style_edition=2024,skip_children=true tests/host_initialization_workflow.rs
    rustfmt --check --edition 2024 --config style_edition=2024,skip_children=true tests/host_initialization_workflow.rs
    git diff --check
    git add -- tests/host_initialization_workflow.rs
    git commit -m 'test(host): exercise legacy web settings on real host'
    git commit -m 'test(host): reference Method associated constants correctly'

以上两个commit分别在对应修改暂存后运行，实际都exit0；不是对同一暂存内容连续提交。受测后仅文档git diff --check0、git add准确单文件和git commit -m 'docs(cli): record real host legacy web compatibility evidence' exit0；Rust差异核验0，具体回执均收录。未跑最终fmt门禁。

环境保持0.3：rustc1.98.1、cargo1.98.1、rustfmt1.9.0、MSVC14.43.34808和SDK10.0.22621.0只读；没有安装/更新。TMP/TEMP为本人树.local/tmp，CARGO_TARGET_DIR为.local/target，所有日志/fixture/target仅E。RUSTUP_AUTO_INSTALL=0、CARGO_NET_OFFLINE=true、DEV/TEST_DEBUG=0、CARGO_INCREMENTAL=0、CARGO_BUILD_JOBS=2；仅本地mock，无release、paid live或push。

## 4. 原/发布证据与历史保全

新原件位于本人树.local/logs/下next15-components-attempt3-fallback、next15-components-attempt4-fallback、next15-fallback-candidate、next15-fallback-fix、next15-fallback-freeze，另含.local/next15-component.ps1驱动。发布清单：[NEXT-15证据/0.4/manifest.json](NEXT-15证据/0.4/manifest.json)。

共26对原/发布文件：18对原样，8份仅发布副本相对化运行时路径；清单逐件列原/发布SHA256、长度与路径变更别名，并绑定七个C授权文件SHA256及Git blob。原件不改，原hash仍为原事实。run-index.json标注为本人转录运行索引，不能替代原Cargo日志。

manifest SHA256：D16946F762FC4A703BFF2790CB29D8ED2CE06BAAF503F743F26B870543EC609E。

0.3和其24对证据保持，manifest SHA256仍为86B28CACA153F68DFC8FFF0790AF96365C4873B5475FEB5DBF634B9858E6EAA0。0.1/0.2及所有attempt1断言失败/旧工具问题同样保留，没有清理旧树或日志。此次只读定位时有不存在路径和rg正则读取错误，未引发产品进程或修改；实际修改前已核实真实server/engine/JS路径。唯一动态失败是上述attempt3测试编译失败。

## 5. 当前冻结与下一第一操作

2026-09-28T15:24:05.6282049Z本人实核dd2a66c9bfdde0a85f0831a7bcf8d1f7bc38e521完整HEAD/两tree/分支，status/index均空，限定本树执行路径/命令行的cargo/rustc/rustfmt/clippy-driver/Host/tests扫描为空，之前所有组件工具进程已退出。**从本节点持续source/index clean/no-build冻结，供经理审查和串行整合。** 人员身份与报告收尾不解除源码冻结。

现无C组件产品阻塞，不代表经理已验收。I11其余旧功能完整回归、B/D最终组合和最终五门禁未执行。下一第一操作：经理合后本人实核新完整HEAD/两tree/status/index；只有经理固定完整组合并另行放行后，才由C唯一串行check→test→fmt→clippy→wasm-check，失败立即停止，源码变化从check重起。没有自动跨对话消息、主仓index操作或自行merge。