# NEXT-15提交报告0.3：真实Host崩溃与安全联测

员工D，GPT-6-astra；2026-09-28；执行ID D-R13-01-20260928-E01，同任务接续。

本人完成授权的输入/I04/I08/I09组件实现与实测：固定候选上输入10/0、I09 3/0、安全7/0；合计Cargo原始20/0，含4个辅助入口，扣除后16个实质顶层测试。限定Rust2024格式与库/安全integration目标Clippy均退出0。本报告不是APPROVED，不替代经理审查或C唯一最终五门禁。

[0.1报告](NEXT-15提交报告0.1.md)、[0.2报告](NEXT-15提交报告0.2.md)、全部旧失败/原始证据保持。0.2的未执行是当时事实；本报告新增真实组合后的运行结果，不回写旧报告。

## 1. 合后核收、完整候选与文件边界

经理完成普通B到D组合后，本人2026-09-28T15:15:26.1617134Z核收：

| 项目 | 完整值 |
| --- | --- |
| 合后HEAD | 95c34b61038f409bf91d7a6082f889212e1a11a7 |
| 合后NoManCode tree | 0dba4fe3deb896f52a9e12f72db42e4e99c7ae64 |
| 合后Rust tree | 3aa3171157cd145fd117f12a42369be0ef1d8d6a |
| 当前候选HEAD | cb5e20d9cac1dcb2747eadd3e516a9945b539ab7 |
| 当前NoManCode tree | 7715c222077ce1c0bdbc084faf5e32e704d23b47 |
| 当前Rust tree | ce3c7c470d9eee59860dc703f8570f22cb66b2cd |
| 分支 | codex/d/credential-input |
| 提交标题 | test(host-config): verify real crash recovery and credential security |

本人核收分支、完整HEAD/两tree、status/index/unmerged均符合后才解除上次冻结。真实B API、C router/CLI与B唯一lib注册已在该组合；没有自己merge、使用假桩或借用旧binary。

相对合后基线仅两个授权文件：

| 文件（相对NoManCode/rust-app） | 变更 | SHA256 |
| --- | --- | --- |
| src/host_configuration_crash_tests.rs | 修复扫描时机，保留真实停点、kill+wait及全部安全断言 | EAD2CE7AE36002C67E8AD8A6854C5635C9ABC0E51E02A8BC9431F50848C25601 |
| tests/host_configuration_security.rs | 新增真实Host/CLI安全integration | 0DDC41225FB25F260E59267501EDC63F1479FD3FF8103A2DA3C690A432E2EA35 |

credential_input.rs仍为7F414E74E16ABB40C81981F3900F483944B673D2C43A5C2D674A2D5DC7445101；Cargo.toml仍为622D465A7C3B542B71D8CD31F092E1240E13A9DCC452D1B92125722D964DBB68；Cargo.lock仍为D8C5CECFD62A03EDB49EF29F18B2BE6063A2B27D7AD292BCB63B11F8D7AB8519。合入B的lib.rs为C17D24BFF15AB16F1D07ECBB5C9538915D9FEE1622E2CA33C7B8C7B31D9B9588，D未修改。未创建旧integration crash文件，未改B/C产品实现。

## 2. 固定候选实际命令及结果

以下命令全部在本人E工作树NoManCode/rust-app执行，元数据记录完整开始/结束UTC、HEAD/两tree、status和受测文件前后SHA256。固定运行均从cb5e20d9…开始并以同一完整SHA结束，source/status/index始终干净；不是用未提交代码成绩冒充候选成绩。

| 精确组件命令 | 退出码 | Cargo结果及计数边界 |
| --- | --- | --- |
| cargo test --locked --lib credential_input:: -- --test-threads=1 | 0 | 10通过、0失败；2 helper，8实质；真实私有console/恢复及真实pipe/detached子进程 |
| cargo test --locked --lib host_configuration_crash_tests -- --test-threads=1 --nocapture | 0 | 3通过、0失败；1 helper，2实质 |
| cargo test --locked --test host_configuration_security -- --test-threads=1 --nocapture | 0 | 7通过、0失败；1 helper，6实质 |
| 现有stable rustfmt --edition 2024 --config style_edition=2024,skip_children=true --check src/credential_input.rs src/host_configuration_crash_tests.rs tests/host_configuration_security.rs | 0 | 只检查D三个文件，不递归格式化B/C |
| cargo clippy --locked --lib --test host_configuration_security -- -D warnings | 0 | 库及D安全integration目标；不是全workspace/all-targets门禁 |
| git diff --check；git diff --cached --check（提交前） | 0 | 仅授权差异，无空白错误 |

固定运行UTC窗口：输入15:39:38.2881133至15:39:59.9398289；I09 15:40:27.1605618至15:40:30.674508；安全15:40:31.3321611至15:40:45.437835；格式15:40:46.1638493至15:40:46.2471443；Clippy 15:40:46.9163399至15:40:48.0402323。日期均2026-09-28。

环境：CARGO_NET_OFFLINE=true、CARGO_INCREMENTAL=0、CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_BUILD_JOBS=2；target/TMP/TEMP/log都在本人树.local/next15-security-03。读取已安装MSVC 14.40.33807与Windows SDK；未安装更新。格式器明确使用已安装stable rustfmt 1.9.0-stable（48a229ceae 2026-09-01），没有继续使用PATH中的旧1.7.1。实际工具绝对路径保留在原元数据，发布副本以现有工具链占位符表示。

## 3. I09真实进程与双重启证据

源码入口：host_configuration_crash_tests的before_commit_kill_wait_restores_all_old_state_twice和after_commit_before_response_kill_wait_restores_one_receipt_twice；host_process_child仅helper。

current_exe来自本次cargo test库binary；精确子入口创建真实Store、Engine、C router/App、HTTP监听和随机token。经bootstrap和认证HTTP建立两路由至revision2。B cfg(test)停点仅设在子Command，父环境不改；在ready核对原Child PID，确认原PUT尚未得到响应，再对持有的实际Child执行kill+wait。

固定候选的原始生命周期证明：

| 停点 | 原Host PID | 原PUT在停点 | 实际终止 | restart-1与restart-2 |
| --- | --- | --- | --- | --- |
| before_commit | 45692 | http_response_finished=false | kill_wait，reaped=true | revision2；原操作receipt数量0；旧配置/凭据/所有快照保持 |
| after_commit | 35252 | http_response_finished=false | kill_wait，reaped=true | revision3；原操作receipt数量1；新配置/凭据/唯一回执持久化；第一次恢复显式原key重放保持原回执 |

每阶段seed、at-stop、restart-1、restart-2共4个实际Host进程，合计8个都显式kill+wait。被kill的child exit_code=1是预期终止状态，不能误读为父测试失败；父Cargo命令退出0。每阶段各完成两次真实Host重启；两阶段Provider业务计数始终0，另有独立健康端点正对照。

停点处仍以一致SQL快照验证可见状态；真实原始DB/WAL/SHM与日志字节只在kill+wait后、打开扫描器的新SQLite连接前采集，避免Windows byte-range lock。所有已知秘密、token、裸秘密SHA256、DPAPI解出的请求指纹及大小写hex断言都保留。不是忽略文件读失败或跳过WAL，也不是drop Arc冒充崩溃。

[生命周期汇总](NEXT-15证据0.3/logs/fixed-crash-lifecycle-summary.json)包含所有PID、kill/wait和restart事件；原始生命周期SHA256：after_commit为7AF706534EB6A284370EA20DDFE9759F179D2285D97F0B9A0A9FFB5DE54F7306；before_commit为4F5FFADBC3BCDA2C2D1785767B4FC55E5719773798E6B30478D89D1E23BAEC51。

## 4. I04/I08及非空保护矩阵

以下全部来自tests/host_configuration_security.rs真实运行，不把helper空返算实质覆盖。

| 实质入口 | 实际覆盖 |
| --- | --- |
| i04_environment_namespace_dpapi_and_http_in_isolated_process | 合成env仅在独立子Command设置；env keep保持来源；replace清key_env且在旧env仍存在时实际Engine选择新DPAPI秘密；旧raw-ID及account密文/rowid保持；新增keep、变base keep、保留命名空间拒绝；损坏DPAPI真实不可解密时keep/replace均fail closed且无DB变化 |
| i04_legacy_keep_and_detached_prompt_preserve_namespace | 实际DETACHED_PROCESS CLI的Prompt返回credential_unavailable/retryable=false，不fallback读pipe、无PUT、无库变更；legacy keep在human/JSON两模式保持来源、revision、config及原namespace/密文 |
| i04_rejected_pipe_inputs_make_no_put_or_db_change | 11类输入×human/JSON共22反例：EOF、空行、首尾空白、多行LF/CRLF、tab、Ctrl+C字节、NUL、坏编码、4096字节超限；带独立合成敏感标记的输入不输出，无PUT、无DB变化 |
| i08_real_commits_then_hostile_responses_are_unknown_without_retries | 真实CLI→本地响应干预层→真实Host完整PUT/commit后，11模式×human/JSON共22反例：丢响应、截断、超1MiB、307、错receipt身份、2xx秘密/token反射、重复JSON键、非法JSON、恶意错误消息、stdout关闭。全部operation_result_unknown/exit1/retryable=false（JSON明确字段），两流无secret/token，干预层及Host都只收到一次PUT；另行显式lookup和用户式同key重放仍为唯一原回执，DB逐值不变 |
| i08_lookup_404_during_received_put_does_not_resolve_unknown | 干预层已完整收到PUT、尚未转交Host时，真实Host lookup为404；实际CLI退出1、host_config_not_found/retryable=false并提示仍在途/不要换key；没有新PUT或新key。随后放行真实commit并丢响应，原CLI仍unknown，Host只有一次提交 |
| configuration_preserves_nonempty_unknown_budget_old_task_and_grants | 真实已完成Task产生usage5，另一次真实project_route_check在Provider已收到completion后取消并恢复成unknown。旧预约held100/unresolved1/spent5，两Project授权、旧Task/绑定及全部非配置表逐值/rowid保持；metadata keep不改grant，replace只使原grant stale、不清除、不释放预算；配置阶段无新增Provider调用 |

Provider计数正对照：非空fixture真实调用3次（1 models+2 completions），明确区分探测和completion；配置前后计数都为3。其它安全/配置场景业务调用0且有健康探测。构造unknown使用真实请求取消+await/recover，明确不是I09崩溃证明；I09另有上述kill+wait进程证据。

响应干预层转发真实Host成功结果后再破坏响应，不复制事务实现、无stub Store、无旧binary。显式同key重放属于测试随后发出的新用户动作，与CLI自动重试分开计数。每次原CLI写操作只发一次。

秘密仅为本轮随机合成值，不经argv或fixture JSON传递。真实DPAPI开封比较不在断言打印秘密；保留数据库/日志扫描秘密、token、裸秘密摘要和封装前请求指纹。环境测试只有私有child变量，未全局修改用户环境或控制台。

## 5. 首失败及修复历史（全部保留）

| 原件/阶段 | 真实失败与修复 |
| --- | --- |
| i09-environment-discovery-failure.json | 既有MSVC自动发现退出1，cargo尚未启动；读取0.1成功环境指向已安装工具链后重试，未安装 |
| i09-attempt1.log/json | Cargo退出101，2通过1失败；after_commit和helper通过，before_commit扫描活跃SQLite文件发生Windows错误33。保留停点SQL断言，文件字节扫描移到真实kill+wait后且先于扫描器开库；attempt2和固定候选均3/0 |
| security-attempt1.log/json | 退出101，4通过2失败：fixture误把models+completion总调用写成2，实际3；另误期望lookup404退出0，真实契约是退出1及不确定性提示。改测试明确1 models+2 completions及404固定错误，不改产品；attempt2为6/0 |
| security-attempt3.log/json | 扩展后退出101，6通过1失败：CREATE_NO_WINDOW仍可能有隐藏console，并不证明无CONIN$，Prompt等待超时。改本测试用DETACHED_PROCESS，超时路径显式kill+wait自己的child，并断言credential_unavailable、无PUT/无回显；attempt4及固定候选7/0 |
| style-format-argument-failure.json、style-format-version-resolution.json | PATH旧rustfmt 1.7.1两次拒绝style_edition，退出1；最初PowerShell引号猜测经工具帮助/版本实核纠正。用已安装stable1.9完成Rust2024样式；原错误不覆盖 |
| 工具调用/补丁准备 | 一次JavaScript转义导致Unexpected identifier $Action，shell未启动；一次扩展patch因格式后锚点不匹配拒绝且未写源码，已在各json记录。均非产品测试失败 |
| fixed-runner-path-failure.json | 固定输入10/0完成后，runner更换cwd使相对脚本路径失效，外层退出1；当时后续组件未启动。改绝对E路径只续跑未执行项，不重复冒算输入数量 |

本轮格式改动仅D-owned文件；最终crash diff没有无关import重排。所有实质失败属于测试前置条件、计数/状态预期或扫描时机；当前未发现需要越权修B/C产品实现的已证实缺陷。

## 6. 原始、发布证据与冻结

原始位置：主仓相对../nhc-d-credential-input/.local/next15-security-03；logs保留每次命令/退出码，tmp保留所有成功和失败fixture，target仅E。原runner保留实际工具链路径。

[证据manifest](NEXT-15证据0.3/manifest.json)记录262份文本证据原始/发布SHA256对照；manifest SHA256为B7031B5847BFBC4FF1539A531666CAAF737DBCB667C0A15C770E4349373BFECB。固定最终运行的30个fixture根、313个原始文件哈希在[fixture inventory](NEXT-15证据0.3/logs/fixed-fixture-inventory.json)。数据库/WAL/SHM等不作为文本发布，保留原E目录，inventory给原hash；文本日志和public生命周期/ready发布副本逐份有双hash。早期失败fixture也保留，未混作最终fixture计数。

发布仅相对化已知仓库前缀和标示现有工具链路径；原件字节不变。发布runner是审计副本，包含工具链占位符，不宣称可直接替代原执行脚本。

2026-09-28T15:41:40.5082711Z（北京时间23:41:40）本人再核完整候选、两tree、branch、source/status/index/unmerged均clean，匹配构建/测试进程列表为空；全部本人命令已结束。从该时刻持续source clean/index clean/no-build冻结供经理串行整合。见[本人冻结回执](NEXT-15证据0.3/logs/final-source-freeze.json)。档案写入与源码冻结分开，不触碰主仓index。

## 7. 未执行及验收边界

未执行全workspace最终五门禁、wasm-check、release、付费live、push；最终门禁仅C获得经理固定放行后执行。本轮限定库/安全目标Clippy不能替代all-targets结果。未安装更新工具链，未reset/clean/stash，未清理旧树或任何失败fixture；无自行跨树merge。

实际DPAPI覆盖同Windows用户的正常保护/解密和坏密文拒绝，不宣称跨用户/OS故障注入；保护目标不是冻结操作系统环境或同用户恶意进程。任意未知秘密埋在公开元数据不在通用scrub保证范围；本轮验证本次已知合成secret/bootstrap token及其指定摘要。局部组件通过不等于整个NEXT-15产品、旧功能全回归或最终验收通过。

下一第一操作：经理review或后续组合返回时先实核本人HEAD/两tree/status/index；只按明确返修项操作。当前候选、源码及本轮报告/身份完成后保持冻结。回执只在当前对话，未自动跨对话发送消息。