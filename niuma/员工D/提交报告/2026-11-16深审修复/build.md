# 2026-11-16 构建与持续验证入口修复

实现者：本轮员工 D。独立树 `../nhc-audit-20261116-build`，分支 `codex/audit-20261116/build`；基线 `bd31edeb80ab6801af6987ee50e719d14f457935`，开工 status/index 空。用户本轮四路开发任务覆盖历史暂停；不接管旧 B/C 草稿，未修改主仓产品或身份。实现提交 `1b0a09b82ff268679b86e7910cea1effc39f7ddc`，由本树实现者普通 commit；整合、主仓身份同步及真实 Cargo 门禁由项目经理负责。

结论：本轮三个范围内问题已实现修复，轻量回归通过，待经理 review/整合。没有宣称真实 release、Rust 全量门禁或最终产品验收通过。

## 触发与反证

1. 原 `build.ps1` 调用 Cargo 时遵循 `CARGO_TARGET_DIR`，分发却固定读取自身 `target/release/peachsh.exe`。先构造旧 target 文件 `stale-default`、既有发布 `published-before` 和本次 mock Cargo 产物 `fresh-host`：绝对目录、相对含空格目录、带 target triple 的三个场景均实际得到错误发布字节。构建失败不覆盖原件这一既有目标继续保留。
2. 原 `test` 仅调用 Cargo。mock Cargo 成功后实际调用记录只有 cargo，网页失败不能影响最终结果；现有 `project-web.mjs` 单独执行 6 组断言通过。缺口是门禁遗漏，不是网页测试本身失效。
3. `rust-toolchain.toml` 为 minimal，仅声明 wasm target，没有日常 fmt/Clippy 所需组件。补充声明，不安装组件、不升级 stable 或依赖、不改 Cargo.lock。此简单声明只做静态检查，没有编写镜像测试。

最初 12 场景夹具在旧实现上 11 失败、1 通过；部分失败还包括新 artifact 格式/入口的预期差异。原始红灯现场保留在本树 `NoManCode/rust-app/.local/build-script-tests/959ce621c400426abb3dbed757da9810`。首个修复 12 场景通过现场为 `1be31e2083234b6eb4f65dfadd290c6c`。最终增加占用发布文件负控及原生非零退出后，13 场景通过现场为 `01f11306553c45248345979eb1e08f63`，同一父目录下每项含 spec、calls、stdout、stderr；这些临时证据不提交到产品树。

## 变更路径与行为

- `NoManCode/rust-app/build.ps1`：Cargo build 使用 `--message-format=json-render-diagnostics`。先检查 Cargo 原退出码，再从本次 `compiler-artifact` 中按 Host 名称、bin 类型、非测试 profile、实际 `src/main.rs` 路径筛选唯一 executable。不推导 target 路径，不扫描旧产物。先复制到发布目录内唯一暂存文件，再同目录覆盖移动；正常异常路径清理本次暂存文件。MSVC/SDK 发现逻辑保持原样。
- 同脚本 `test`：Cargo workspace 成功后才执行 Node 网页测试；Cargo 失败保留其退出码，Node 失败返回其退出码。显式控制原生错误首选项，使调用者 Stop/true 设置不将原生 101 改成泛化异常 1。新增 `web-test` 在工具链发现之前独立执行，不依赖 MSVC/Cargo。
- `NoManCode/package.json`：增加 `test`、`test:web`、`test:build-script`；既有 `test:rust` 使用扩充后的日常门禁。没有新增包或 install 步骤。
- `NoManCode/rust-app/rust-toolchain.toml`：声明 `rustfmt`、`clippy`。
- `NoManCode/rust-app/README.md`、`NoManCode/tests/README.md`：区分日常 Cargo+网页门禁、轻量网页检查、WASM 编译、受控 ignored 矩阵、付费测试及独立 release 分发；说明返回码、target 支持与无 npm 的直接入口。
- `NoManCode/rust-app/tests/build-script.ps1`：新增端到端脚本回归，在独立临时产品树运行被测脚本副本，显式 mock 工具目录和 Cargo/Node，不修改真实 bin 或 target。失败退出使用 `cmd /d /c exit 101` 验证原生非零返回语义，不启动 Cargo 或产品。

生产 Rust、现有业务测试、schema、迁移、权限、凭据及公共产品协议均无变化。

## 实际验证

从本工作树仓库根执行：

| 命令/检查 | 实际结果 |
| --- | --- |
| `pwsh -NoProfile -File NoManCode/rust-app/tests/build-script.ps1` | 最终 exit 0，13 场景全部通过；实际命令耗时约 6.6 秒 |
| `node NoManCode/rust-app/tests/project-web.mjs` | exit 0，6 组原有网页断言通过 |
| `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action web-test` | exit 0，同 6 组网页断言通过 |
| PowerShell AST Parser 读取 build.ps1 和新增测试 | 无语法错误；最终脚本亦经 13 场景实际加载执行 |
| Node 解析 package.json | 成功，新增及兼容 scripts 可读 |
| `git diff --check`、`git diff --cached --check` | exit 0；提交前逐项复核 staged diff，限定 6 个实现文件 |

13 场景分别为：absolute-target、relative-target、target-triple、cargo-failure、missing-artifact、ambiguous-artifact、wrong-package、missing-file、locked-output、cargo-test-failure、web-test-failure、daily-gate、web-only。断言实际发布字节、失败保留旧文件、无遗留暂存、唯一调用及执行顺序、具体退出码、实际网页脚本路径和 target 环境原值；不是仅检查命令字符串存在。

当前 PATH 没有 npm，未声称 npm launcher 实跑；已直接执行对应 Node/PowerShell 命令并解析 package scripts。没有安装 npm、rustfmt、Clippy 或其他依赖。

## 失败、取消、恢复及兼容

Cargo 返回非零时不解析/分发即使已经输出 Host artifact；mock 已确认 101 原样返回且不执行网页。artifact 缺失、歧义、不同源码包、文件缺失均拒绝分发。发布文件被占用时实际覆盖失败、旧文件保留、本次暂存清理。只有 Cargo 与网页均成功才完成日常门禁。

正常异常由 finally 清理本次暂存文件；若构建或复制阶段被外部强制结束，未到替换时旧发布文件保持，可能留下唯一暂存文件。下一次不把暂存/旧 target 当本次产物，不自动扫描清理历史文件。未执行强杀/掉电/产品进程取消实验，不声称这些动态已验证。完成替换后发生的进程退出不回滚已发布完整文件。

相对 target 由 Cargo 在工程工作目录解析；绝对目录和 target triple 的最终路径来自 artifact，无额外猜测。沿用原默认 build、release/locked、原 workspace test 范围和 ignored 语义，未把受控进程矩阵塞入普通测试。`test` 现在需要 Node；仅网页入口不需要 C++ 工具。stable 通道、MSVC 探测与依赖锁均未升级或重构。

## 交给主线程的验证选择与剩余事项

可先在整合候选仓库根复跑两条轻量命令：

```powershell
pwsh -NoProfile -File NoManCode/rust-app/tests/build-script.ps1
pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action web-test
```

真实 Cargo 只由经理串行：进入 `NoManCode/rust-app`，沿既有安装设置 VCToolsInstallDir/WindowsSdkDir，既定独立 CARGO_TARGET_DIR/TMP/TEMP、offline/locked/jobs 约束保持；如沿用已有串行验收，设置 `RUST_TEST_THREADS=1`，再执行 `pwsh -NoProfile -File build.ps1 -Action test`。独立 `check`、`fmt`、`clippy`、`wasm-check` 按经理门禁安排，逐命令记录退出码。需要强制离线时由该验证进程设置 `CARGO_NET_OFFLINE=true`；本补丁不替主线程打开窗口。

本轮没有执行 Cargo/rustc、真实 release、产品或真实进程矩阵，没有访问 Provider。真实 Cargo artifact 到发布复制仍需未来单独发布窗口验证；mock 不代替编译。用户本轮未要求 release 编译，所以不以此阻塞可审阅候选。没有验证其他 target 平台运行能力，Windows triple 场景只证明不会遗漏 triple 子目录。

主线程负责更新继承改动中的身份与环境文档旧限制；本树未越权改动这些文件。历史 unknown 不因本轮脚本测试改判。

## A 安全候选交叉审查（2026-11-16）

按经理追加任务，只读审查 `../nhc-audit-20261116-security` 的 `secrets.rs`、`provider_runtime.rs`、`provider.rs` 差异及相关 Store/Engine 调用链。首次针对未提交实现阅读，随后在 A 提交 `7c657ce37e82744933d1dd2141a1fc34a542d4a3` 再核，以下代码位置与问题仍存在。行号以该候选为准；路径均以该树仓库根为基准。

### D-XA-01 / P1：token 抢先消费敏感赋值字段

- 入口：`NoManCode/rust-app/src/secrets.rs:473`，第 478–480 行输出 `[redacted]`、清空 field 并进入 Token 模式。
- 反例：completion codec 依次 emit `"sk-"`、`"password"`、`"="`、`"opaque123"`，正常完成时返回拼接后的 assistant content。
- 静态推导：流式输出为 `[redacted]=opaque123`。原完整文本策略在 `secrets.rs:232` 先判断 assignment，`sk-password` 经 `normalize_field` 变为敏感的 `sk_password`，应得到 `sk-password=[redacted]`。字段身份先被消费后，实际值未脱敏；单次 emit 完整文本也会出现同类问题。Store 的逐事件策略无法从 `[redacted]` 还原字段身份。
- 建议验证：真实替换 codec → Engine → Store → SSE 链路检查所有事件均不含 `opaque123`，并检查最终 output/message；同时覆盖 token 字段名和普通 token 两类，避免修复破坏既有 token 脱敏。

### D-XA-02 / P1：原始字段截断丢失规范化身份

- 入口：`NoManCode/rust-app/src/secrets.rs:503`，field 超过 128 字节时删除最前字符；识别使用剩余原始字符串。
- 反例：三个 emit 分片依次为 `"api" + "_".repeat(128)`、`"key="`、`"opaque123"`。
- 静态推导：完整字段规范化为 `api_key`，属于敏感赋值；截断后只剩下划线与 `key`，规范化为被 `sensitive_assignment_field` 明确排除的通用 `key`，第三片值原样输出。`store.rs:1023` 对事件分别脱敏，三个事件均缺完整赋值，无法补救。最终消息完整脱敏也不能撤回已写入或发送的事件。
- 建议验证：超过上限的 `_`、`-`、`.` 连续/混合分隔符，camelCase/全大写缩写、Unicode 断点及普通非敏感字段负控。候选“压缩分隔符后保留有界字段状态”需与完整 `normalize_field` 的边界和大小写规则等价，不能只证明缓冲长度有界。

### D-XA-03 / P2：空引号值使事件和最终消息不一致

- 入口：`NoManCode/rust-app/src/secrets.rs:548`，第 550 行在看到开引号时立即输出占位符。
- 反例：两个 emit 分片依次为 `password="` 与 `"`，最终 assistant content 为 `password=""`。
- 静态推导：流式结果为 `password="[redacted]"`；完整策略 `secrets.rs:366` 因值为空不替换，因此保存的 assistant message 为 `password=""`。Engine 由流式回调累加 output（`engine.rs:2026`），而最终 message 按完整文本策略保存，三者不一致。单引号空值也应覆盖。
- 建议验证：延后空值/非空值裁决，新增空引号、跨片闭引号及错误/取消时未闭合引号回归，断言事件拼接、task.output、最终 assistant content 一致。

本次交叉审只读取源码与 Git 状态，未编辑 A 文件，未执行 Cargo/rustc、产品、网络请求或新的动态复现。上述结果是可逐步推导的静态反例，不称为实际 Rust 测试失败。经理已确认收到并转交 A 修复和补行为回归；其“安全 5 专项通过、Windows 7 通过”是经理提供的独立验证状态，本报告不将其扩展为这些反例已关闭。

复审待办：A 修订 Token 模式继续追踪 field、压缩长字段分隔符和空值裁决后，重新检查与完整规范化的等价性；跨 completion 状态丢失由经理另行发现并交 A/B 协调，不冒称 D 的发现，也不由本次静态阅读提前关闭。

## A/B 修订算法复审：消息与事件同链问题（2026-11-16）

经理追加只读复审后，已看到 A 的未提交修订：分隔符压缩后再限制字段长度、Token 模式继续跟踪字段、QuotedStart 延迟空值裁决；B 的 recovery 树在 Engine 中跨 completion 共用单一 redactor，调用 A 的 `execute_with_redactor` 原始 decoder 边界。先前三个最小反例的值泄漏/空引号即时替换在静态控制流中已有对应修正，但不据此宣称最终组合动态通过。以下两项来自当时仍在修改的工作区快照，行号仅作该次定位，不冒充固定最终提交。

### D-XA-04 / P1：安全流式输出之外仍保存跨轮原始 assistant 消息

- 入口：security 树 `NoManCode/rust-app/src/provider_runtime.rs:663` 返回 decoder 原始 output；recovery 树 `NoManCode/rust-app/src/engine.rs:2172` 将原始 message 加入 candidate.messages；`domain.rs:539` 的持久策略逐个字符串脱敏。
- 反例：与 A 新增跨轮测试相同，第一 completion 的 content 为 `password=` 并返回合法只读工具组，第二 completion 的 content 为 `opaque123`。单一流式状态正确生成 `password=[redacted]`，但第二条 assistant message 本身没有字段前缀，独立完整文本策略保留 `opaque123`，因此凭据仍存在持久 Task/messages 中。
- 当时新增跨轮测试检查 task.output、事件和 SSE，没有断言所有持久 messages 均不含该值。单轮 `sk-password=opaque123` 的 `[redacted]=[redacted]` 流式投影与 `sk-password=[redacted]` 完整消息投影不一致，也是双文本来源的具体表现。
- 建议验证：replacement codec 的全 Task/messages 秘密断言；有 delta、仅 finish 输出、无 delta 但有最终 content 三类均保留合法非敏感文本，并核对最后 pending tail 的归属。

### D-XA-05 / P2：逐事件再次脱敏制造不存在的 token 边界

- 入口：security 树 `NoManCode/rust-app/src/provider_runtime.rs:635` 发送 stateful 过滤结果；`NoManCode/rust-app/src/store.rs:1023` 又对每片 data 单独调用 `redact_persisted`。
- 反例：同一 completion 两次 emit `foo`、`sk-plain`，最终 content 为 `foosk-plain`。根据既有 token 边界规则，紧接 ASCII 字母的 `sk-` 不是匹配；有状态过滤正确保留原文。但第二事件再次独立过滤时以 `sk-` 开头，变为 `[redacted]`。SSE 拼接为 `foo[redacted]`，而 task.output/message 为 `foosk-plain`。
- 这是现有可识别策略下的普通文本负控，不要求识别任意未知凭据。建议验证同一 completion 内不同分片边界、跨 completion 的相邻普通文本，以及最终 tail 的持久投影一致性。

经理已收到并明确 A/B 分工：A 负责安全 message 投影、无 delta codec fallback 和全 Task 秘密断言；B 负责可信 Host 流式 delta 的内部 Store 入口，避免再次制造边界，并将最终 tail 写入对应 assistant message。此处仅登记派发，不声明修复完成或测试已通过。最终候选待经理交回后再核相应 diff。

本次仍未编辑 A/B 产品文件，未执行 Cargo/rustc、产品、真实进程测试或外部服务调用；所有结论均为静态反例。复审范围停留在上述既有 token/bearer/assignment 策略及消息/事件一致性，不扩展未知凭据识别承诺。

## 最终交叉审结论：原反例范围关闭（2026-11-16）

结论：`APPROVED（D-XA-01～05 原反例及对应回归范围）`。前文保留发现时的源码快照与待办状态；本节按后续修复和验证更新结论，不改写历史。本次没有发现上述路径的明确剩余阻断，不扩展未知凭据识别能力，也不代表全产品或发布验收通过。

### 版本与证据归属

- 父线程确认受测 HEAD 为 `89e3bb1dfd2cf013571854014580f25aee20e370`；最后 10 项安全专项全部通过，覆盖完整 Task、SSE、codec fallback 和普通 `foo` / `sk-plain` 负控，另可信 delta Store 专项 1/1 通过。这是父线程本轮动态结果，D 没有独立重跑，也未在本次核验原始运行日志、命令或退出码，不将其编造为 D 自行执行的证据。
- D 最后只读复核 A `f81cec7aa536916f23ed4fb7a69f0fd51eb70c0f`（含实现 `26951f042900bf2da322d6535955247c7bed982f`）及 B `b06a1b1baad6e3660d9bbf72e4cb1e406d621a31`。随后对受测 HEAD 做限定 `git diff --quiet`：A 的 `secrets.rs`、`provider_runtime.rs`、`provider_runtime_tests.rs` 三文件无差异，B 的 `engine.rs`、`store.rs`、`repository.rs` 三文件无差异，两次 exit 0。下列路径均相对仓库根，行号绑定受测 HEAD。
- 本次 docs 提交以 D 树 `69e767f2d1fb593330885d8a16a543b28423dee5` 为父提交，仅追加本报告。产品继续冻结；主仓身份、总验收及整合由经理维护。

### 逐项关闭依据

| 原问题 | 受测代码与行为断言 | 范围结论 |
| --- | --- | --- |
| D-XA-01：`sk-` / `password` / `=` / `opaque123` 丢失赋值字段身份 | `NoManCode/rust-app/src/secrets.rs:590` 的 Token 分支继续 `track_field`；同文件第 752 行测试精确四片反例及 token 形状字段的所有分片边界，期望 `[redacted]=[redacted]`。`provider_runtime_tests.rs:385` 另覆盖跨 completion 的同类字段身份。 | 原值泄漏关闭；不要求敏感字段名必须保留为原始字面文本。 |
| D-XA-02：长分隔符使 `api_key` 被截断为普通 `key` | `NoManCode/rust-app/src/secrets.rs:470` 先压缩连续分隔符再限制字段状态；第 771 行起覆盖 128/1024 个分隔符、混合分隔符、整体及分片输入，并与完整 `redact_text` 比较；第 797 行覆盖 4096 个分隔符及状态上界。 | 原长字段反例关闭；不是任意未知字段识别保证。 |
| D-XA-03：空引号值提前输出占位符 | `NoManCode/rust-app/src/secrets.rs:615` 的 QuotedStart 先判断闭引号；第 790 行空值断言及第 791 行非空负向区分仍在。`provider_runtime_tests.rs:386` 跨轮空值由后续完整 Task/assistant/output/SSE 断言验证。 | 原空引号不一致关闭。 |
| D-XA-04：跨轮 output 安全但原始秘密仍进入 assistant messages | `NoManCode/rust-app/src/provider_runtime.rs:640` 累积同一 redactor 的安全输出，第 672 行无 delta fallback 继续同一状态，第 688 行替换返回 message.content。`engine.rs:2174` 仅工具组先保存；最终文本分支第 2209 行结束 redactor，第 2217 行补消息 tail，第 2223 行才首次持久保存最终消息。`provider_runtime_tests.rs:378`、第 419 行及第 473 行分别覆盖跨轮成功、失败/取消和有/无 delta codec；第 402/466/503 行检查整个 Task 不含测试秘密，第 409/501 行检查 assistant 内容。 | 原持久消息泄漏关闭；最终成功 tail 不再遗漏于首次保存，错误/取消路径不提前 flush。 |
| D-XA-05：逐事件重滤制造 `foo` / `sk-plain` 的假边界 | `NoManCode/rust-app/src/engine.rs:2109` 与第 2211 行通过 `Store.completion_delta`；`store.rs:1115` 为 crate 内部入口；`repository.rs:1728` 限定文本 delta，转第 1952 行公共关系校验，不再逐片重滤。普通 `append_event` 仍在第 1947 行过滤。`provider_runtime_tests.rs:509` 检查普通跨轮片段的 output、事件/SSE 回放；`store.rs:4340` 专项检查新旧任务、公开入口脱敏、kind/shape/关系拒绝及重开数据库后文本。 | 原 output 与事件/SSE 不一致关闭。 |

普通片段负控的范围是拼接后的 task.output、持久事件和 SSE；独立 assistant 消息仍经过既有 `safe_task_value` 策略，可能对单条消息开头的 token 形状文本保守脱敏。`provider_runtime_tests.rs:520` 已明确这一边界，不将负控通过解释为取消消息持久化策略或要求每条消息与原始 codec 文本逐字相同。

### 未执行与后续验收

本次只读代码、Git 版本及测试断言，修改唯一文件为本报告；没有运行 Cargo/rustc、产品、真实进程测试或外部服务调用，没有修改 A/B 或整合树。没有新增功能、schema、迁移、权限或凭据策略变更。

父线程在上述受测 HEAD 开始六项门禁并冻结产品；截至此结论，六项门禁仍在验收中，未记为通过。D 无上述原反例范围内待修事项；后续由父线程记录门禁原件、最终整合与总验收。历史 unknown、真实 release 分发及未覆盖的其他范围不因这次专项关闭而改判。

## 最终构建与证据脚本静审（2026-11-16，在途快照）

经理按用户提速四路并行指令再次派发 D，只读核对 integration 的 `.local/audit-20261116/run-gates.ps1` 与 `publish_evidence.py`，检查最终测试口径、原件哈希、失败 attempt 保留和 ignored 边界。本次结论为 `CHANGES_REQUIRED（证据发布脚本两处校验缺口）`；不撤销上一节对 D-XA-01～05 的限定结论，也不修改产品或父线程本地脚本。

### 固定审查输入

- 读取工作树为 `../nhc-audit-20261116-integration`；复核时 HEAD 为 `ed98a606d2d10dd8d1f9ef5ae0a49605e962dde2`，`git status --short -- NoManCode` 空。最终 attempt1 是较早的 `89e3bb1dfd2cf013571854014580f25aee20e370`，不把当前 HEAD 冒充其受测版本。
- 以下脚本路径均以整合树仓库根为基准。`NoManCode/rust-app/.local/audit-20261116/run-gates.ps1`：4077 bytes，SHA-256 `7d6c8d5978517ff8fa66bf719b5c18340a9198c824e06fa9b38e158e35c3445a`；`publish_evidence.py`：3347 bytes，SHA-256 `40a347238c0758b589314ebf79de157259b8bba8c6c884777b1dc2025e78be71`。行号绑定这次原件快照，两脚本均属于被忽略的本地执行材料，不因 HEAD 相同自动获得版本绑定。
- 只读脚本审查期间，经理给 runner 增加了 `worktree_timing` 记录；本节固定的是含该字段的上述版本。D 未修改任何父线程材料，未参与 B 的超时根因诊断。

### 两处发布前需修正的问题

**D-EV-01 / P1：最终工作区检查失败仍可能留下可发布的成功 manifest。**

- 位置：`NoManCode/rust-app/.local/audit-20261116/run-gates.ps1:64` 至第 68 行；`NoManCode/rust-app/.local/audit-20261116/publish_evidence.py:12`。
- 触发：六项命令均 exit 0，但执行过程中产品出现未提交变化，HEAD/product tree 未变。runner 第 64–66 行先把六个成功结果、final_product_tree 和 ended_utc 写入 manifest，随后第 68 行才因 dirty 状态抛错。
- 影响：publisher 只要求六项 exit 0、前后提交树一致；该失败路径生成的 manifest 仍满足第 12–13 行条件，可以发布为最终门禁证据。Git tree 哈希不能代表未提交文件的实际字节。本次只读未发现该情况实际发生，当前产品 status 空；问题由代码的具体先后顺序证明。
- 建议：最终树/工作区检查通过后再写显式 `status=passed` 或等效完成回执；检查失败写 `status=failed`，发布端必须验证最终成功状态。完整性检查及明确门禁名称集合校验均应在创建正式发布目录前完成。以“六个命令成功、末尾 dirty 检查失败”为负控，确认禁止发布。

**D-EV-02 / P2：发布端未把实际日志字节与门禁记录的哈希绑定。**

- 位置：`NoManCode/rust-app/.local/audit-20261116/publish_evidence.py:39` 至第 48 行，结合第 12–13 行和第 50–57 行。
- 触发：某一门禁结束并记录哈希后，原日志被截断、编辑或替换；例如 `check.log` 被改写，而 test.log 仍保留全通过结果。发布端对当前日志重新计算 source/published 哈希，但从未比较 `manifest.gates[*].sha256`、`bytes` 与此次读取的原件。
- 影响：发布清单会同时带上历史门禁哈希和另一份原件的新哈希，仍输出最终摘要。对非 test 日志没有任何内容核对；对 test 日志仅核部分正则结果，也不能证明它就是产生门禁退出码的原件。本次现存原件校验全部匹配，不称已有日志损坏。
- 建议：复制前按六门禁固定名称逐一读取日志，要求 SHA-256/字节数等于 manifest；缺失或不匹配即拒绝发布。同一份已校验的 bytes 用于规范化、写副本和生成双哈希，避免另读一次换了原件。专项日志的旧清单也应做同类核对；有意保留的空日志/失败日志按其真实状态记录，不能要求它们冒充成功。

两项已在本轮聊天即时反馈经理；D 没有擅自修脚本。后续若经理修改，需要按新的脚本原件哈希复核，不能把本结论直接套到新版本。

### 实际原件校验与测试口径

- 用只读 Python 解析 JSON、日志并计算 SHA-256/字节数，未导入或执行 publisher。attempt1 的 check/debug-bins/test 三份日志与 manifest 全部匹配；`intermediate-results.json` 的九项专项日志也全部匹配，包括失败的 `windows-workspace-tests`，没有只验证成功项。
- attempt1 manifest 固定 `product_tree=84e03b3a44024ef2114241a8613b4955746d8fa3`、`rust_tree=13fda626af066d1932ca1fcc56a72585be7bfc8c`；check/debug-bins exit 0，test exit 101。其 test.log 为 93536 bytes，SHA-256 `c989d0886c9461c3074ce3a0b2a5af3edfee037735fe32a59521965434712208`。失败原件确实保留 `worktree_http` 的 TimedOut 和第 351 行 unwrap；不把后续专项通过覆盖为 attempt1 全过，也不在本报告认定超时根因。
- 对 attempt1 已输出的 59 个 Rust `test result` 块求和为 **875 passed / 1 failed / 25 ignored**，这是提前失败的部分结果，不能称最终全量结果。25 个 ignored 明确包含 24 个受控进程/工件用例及 1 个需要测试密钥的付费模型用例，均未执行，不并入 passed。没有 `--ignored` 或 `--include-ignored` 的最终 gate；`build.ps1:90` 的独立 live 入口没有被 runner 调用。
- `build.ps1:85` 的日常 test 先执行 `cargo test --workspace --locked`，第 87 行失败立即退出，再到第 88 行 Node。故 attempt1 的网页 PASS 组数为 0，网页检查未执行；fmt、Clippy、wasm-check 也尚未执行。`check`/debug build/wasm-check 的编译成功不计为测试通过。
- publisher 第 50–63 行只从选定最终 attempt 的 test.log 汇总 Rust passed/failed/ignored，并把网页 PASS 组单列；没有把历史 attempt 或专项的数量累加到最终 Rust passed。此统计分离方向正确，但仍须先解决上面两项原件/终态校验问题。
- publisher 第 64 行 `build_mock_scenarios=13` 为硬编码，不能证明本次最终门禁重跑过 mock。本次核其独立 `build-script-tests.log`，确有 13 项 PASS；中途清单记录 exit 0、受测 HEAD `0eb7cac849cfc303ec65e5888f4a24db2c93df6d`，日志哈希/字节数匹配。`git diff --quiet` 确认该 HEAD 到当前 HEAD 的 build.ps1 与 tests/build-script.ps1 无变化（exit 0）。可作为独立、相关源码一致的既有证据保留；最终报告应标清来源与受测版本，不能将 13 并入最终 Rust/网页总数，建议摘要从已核清单派生该值而不是固定写死。

### 保留策略与限制

runner 第 27 行创建新的 attempt 目录，不带 Force，同名目录存在即拒绝，正常执行不会覆盖旧 attempt；每项命令结束先落 manifest 再遇非零退出，因此当前失败 attempt1 已有完整三项记录。publisher 第 33–36 行会复制其他 `final-attempt*` 目录直接包含的 `.log` / `.json`，第 65 行独立列其名称；只从选定 attempt 汇总结果。该范围不自动包含临时数据库、深层子目录或其他类型工件，报告不能说已发布全部失败现场。原件仅被读取；发布副本作工作树前缀规范化并分别计算 source/published 哈希，旧原件哈希仍应保留。

此次没有执行门禁、Cargo/rustc、产品、进程测试、publisher 或外部服务；Python 仅做现有文件读取、摘要解析和哈希计算。修改仅为 D 本报告补充，普通 docs 提交交经理整合；身份及总验收仍由经理同步。最终六门禁、B 超时诊断和修订后的发布流程仍在进行，不在本报告预写通过。
