# NEXT-15提交报告0.2：真实Host崩溃测试候选准备

- 员工D，GPT-6-astra；2026-09-28；执行ID D-R13-01-20260928-E01，同任务继续。
- 已增量完整读取本人[经理review0.1](../../审查记录/第十三轮/经理review0.1.md)及[冻结补充02](../../../项目经理/任务/2026-09-28NEXT-15冻结补充02-真Host崩溃测试接线.md)。
- 结论：唯一新增测试文件已提交；**尚未编译、类型检查、Clippy或运行I09。完整D任务和安全联测未完成，不自行宣称APPROVED。**
- review0.1的APPROVED仅限原输入组件；[报告0.1](NEXT-15提交报告0.1.md)、旧证据及历史失败全部保留。

## 1. 固定候选与范围

| 项目 | 完整值 |
| --- | --- |
| 分支 | `codex/d/credential-input` |
| HEAD | `a5a4ac64fe2d6b9175afada7bfde4f796c19fe13` |
| NoManCode tree | `7c7b9e51dfaade60f0337eac214c452179302cb3` |
| Rust tree | `f28f6fae4decf583f2732d497a85e1e66515a989` |
| 父提交/输入候选 | `3675e3afeebb7c6ce73200a888588775bc55e3e9` |
| 原开发基线 | `5194099ee4d1eb72af5f286826c72c6fd19d29e8` |
| 提交标题 | `test(host-config): prepare real HTTP crash recovery harness` |
| 唯一增量 | 新增 `NoManCode/rust-app/src/host_configuration_crash_tests.rs`，817行 |
| 新文件SHA256 | `DDA1CE2AEE0BD9A463E1547CCB0026D77D024FF74B3C11AD9202052DE513BD70` |

源码只本人工作树 `../nhc-d-credential-input`；档案只活动主仓本人目录。本轮未创建旧计划 `tests/host_configuration_crash.rs`，未改lib.rs或B/C源码，未自行merge，未更改依赖或Cargo.lock。主仓index和档案提交仍只由经理操作。

此前文件哈希保持：

| 文件 | SHA256 |
| --- | --- |
| credential_input.rs | `7F414E74E16ABB40C81981F3900F483944B673D2C43A5C2D674A2D5DC7445101` |
| Cargo.toml | `622D465A7C3B542B71D8CD31F092E1240E13A9DCC452D1B92125722D964DBB68` |
| Cargo.lock | `D8C5CECFD62A03EDB49EF29F18B2BE6063A2B27D7AD292BCB63B11F8D7AB8519` |
| lib.rs | `FC38A7144768ED4D90E82230A13AFFE437C54F4206610BA316847864EC28B84D` |

## 2. 测试准备内容（代码事实，全部尚未执行）

Windows库内模块带 `#![cfg(all(test, windows))]`。父进程用本次 `current_exe()`和模块路径构造精确helper入口。子进程创建实际Store、Engine、C的router/App、随机Host token和loopback HTTP监听；父进程经bootstrap取得本次token并执行真实认证HTTP，无token配置请求断言403。

B的两停点变量只设在子Command：`NHC_HOST_CONFIG_TEST_ROOT`、`NHC_HOST_CONFIG_TEST_PHASE=before_commit|after_commit`；恢复子进程移除两变量，父进程不修改全局env。父进程核对ready内PID属于实际Host Child，确认PUT尚无响应，执行该句柄的kill+wait，并要求被打断PUT成为传输错误。Child guard在超时/panic时也kill/reap，保留fixture和日志。

| 新实质场景 | 预期断言，尚未运行 |
| --- | --- |
| `before_commit_kill_wait_restores_all_old_state_twice` | 真实HTTP建立两路由至revision2；commit前停点kill+wait；连续两次重启真实Host，旧revision/路由/凭据/请求及受保护表快照不变，无新回执。 |
| `after_commit_before_response_kill_wait_restores_one_receipt_twice` | revision2发起更新，commit后响应前停点kill+wait；两次真实重启都为revision3且仅一份原回执；第一次恢复重复原key/body，验证幂等回执及行状态。 |

另1个 `host_process_child` 是辅助入口；普通测试进程无子环境时直接返回，未来计数应单列，不能算第三个实质场景。

已写入的其他断言和记录：

- 只读SQLite快照覆盖config、secrets、host_config、network和budget相关表，包含rowid及类型化值；实际DPAPI解密结果与合成预期秘密比较，断言不打印秘密。
- 扫描DB/WAL/SHM和保留日志中的合成秘密、Host token、秘密摘要和请求指纹明文/编码；不读取真实用户密钥。
- 本地mock Provider独立健康入口提供启动正对照，业务请求计数预期0。PUT无proxy、无重定向、禁重试。
- 每case在TMP下保留 `nhc-i09-<phase>-UUID`；子stdout/stderr及生命周期JSONL记录PID、ready、停点、kill/wait、revision/计数；秘密不经argv或fixture JSON。

覆盖限制：当前network/budget保护快照来自受控空基线，**尚无非空grant/budget/unknown fixture**；不能把空表相等宣称为非空unknown回归。独立环境覆盖、恶意响应泄露和未知结果等完整安全矩阵仍待依赖整合后补齐。没有Store-only child、复制生产事务、递归cargo、寻找旧可执行文件或drop Arc冒充崩溃。

## 3. 依赖现状及owner接线

D树尚未合入B真实Host、C必要接口和B唯一lib注册，因此不执行编译或产品测试。2026-09-28T15:01:56.0696539Z只读观察B HEAD `c94a48daded1b0098b560e6f8761d64a7e8c1096`、C HEAD `77ea5a6d42f0894b1305b07306a7daaa8eccbcad`，当次status均空；这是观察，**不是owner冻结回执或D受测基线**。原命令/退出码、接口文件hash在 [dependency-observations.json](NEXT-15证据0.2/dependency-observations.json)。

代码接线依据是B事务commit前后的 `crash_probe::hit`（写PID并阻塞），以及C的 `/api/bootstrap`、`/api/host/config`、`/api/host/routes/{id}`、`/api/host/config/lookup` 和 `x-peachsh-token`。

经理串行整合依赖及本候选，B唯一维护以下lib注册：

```rust
#[cfg(all(test, windows))]
mod host_configuration_crash_tests;
```

依赖齐备后D先实核完整HEAD、两tree、status/index和文件hash，再按固定组合开展组件调试。停点或接口问题给经理具体入口、反例与建议，由原owner修改；D不越权修B/C。

## 4. 实际检查、失败与未执行

格式检查cwd为本人树 `NoManCode/rust-app`；候选Git操作cwd为本人树根；shell均显式E盘cwd。原绝对路径留在原件，发布副本仅相对化仓库前缀。

| 原始命令/操作 | 实际结果 |
| --- | --- |
| `rustfmt --edition 2024 src/host_configuration_crash_tests.rs` | 退出0，仅授权文件。 |
| `rustfmt --edition 2024 --check src/host_configuration_crash_tests.rs` | 退出0，仅解析/格式，不是类型检查。 |
| `git diff --check` | 准备期与提交后均退出0。 |
| `git diff --cached --check` | 提交前退出0。 |
| `git commit -m ... -m ...` | 退出0；完整原参数数组和原输出在candidate-freeze.json/candidate-commit.log。 |
| `git rev-parse HEAD HEAD:NoManCode HEAD:NoManCode/rust-app` | 退出0，完整三值见§1。 |
| `git status --short` | 退出0，输出空。 |
| `git diff --cached --name-status` | 退出0，输出空。 |
| `git diff --name-status HEAD^ HEAD` | 退出0，仅授权新增文件。 |
| B/C只读 `git -C <tree> rev-parse HEAD`、`status --short` | 各退出0。 |

静态检查窗口2026-09-28T14:57:54.1171243Z至14:57:54.5000462Z；候选冻结实核2026-09-28T15:01:12.1532954Z（北京时间23:01:12）。

本轮上述检查未见失败。档案写入阶段一次工具参数解析失败：`SyntaxError: Unexpected identifier 'codex'`；原因是JavaScript模板中Markdown反引号未转义，shell未启动、报告未落盘、无shell退出码、不影响源码。保留 [report-write-tool-failure.json](NEXT-15证据0.2/report-write-tool-failure.json)，改为普通字符串patch后重新写入。历史0.1所有失败证据不改。

**未执行**：cargo编译/类型检查、Clippy、真实Host崩溃场景、DPAPI联测、完整安全矩阵、I09、最终五门禁。本轮产品测试计数无结果，不填0通过，也不把0.1成绩挪作联测结果。

整合后建议组件命令如下，**本轮未执行**；届时CARGO_TARGET_DIR、TMP、TEMP和日志全部置本人E树：

```powershell
cargo test --locked --lib host_configuration_crash_tests -- --test-threads=1 --nocapture
```

最终五门禁仍仅C收到经理放行后执行。现有工具链只读，不安装更新。

## 5. 原始/发布证据与持续冻结

原件位置（主仓相对）：`../nhc-d-credential-input/.local/host-crash-preparation-02/logs/`；本轮TMP/TEMP是其父级 `tmp/`，未启动构建，无新target输出。

[NEXT-15证据0.2/manifest.json](NEXT-15证据0.2/manifest.json)记录16份原始/发布SHA256对照。原件保留，发布副本仅仓库前缀相对化；原始/发布不同hash及转换说明逐份保留。空日志只表示无输出，退出码来自元数据。

固定提交后source/status/index clean，本轮未启动cargo/产品测试；冻结时可见匹配构建/测试进程列表为空。进程快照仅描述实核时刻；本人不再启动构建，持续clean/no-build供经理整合。候选和本人0.2档案完成后保持冻结。无主仓index操作、跨树merge、reset/clean/stash、release、付费live、push、工具链安装更新或旧树清理。

下一第一操作：经理整合后在本人E树重新核HEAD/两tree/status/index；依赖和B注册齐备才开展明确范围的安全/I09组件调试。回执及报告只回复当前对话，不自动跨对话发消息。


补充本人冻结回执：2026-09-28T15:06:25.3748440Z再次核验完整HEAD/两tree及source/status/index clean，明确持续no-build；源码冻结与档案发布独立。见 [continuous-source-freeze.json](NEXT-15证据0.2/continuous-source-freeze.json)。经理已告知静态接线可进入联测，该消息不视为I09动态通过。
