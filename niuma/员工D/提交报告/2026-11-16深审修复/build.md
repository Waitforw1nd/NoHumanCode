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
