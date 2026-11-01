# NEXT-21W2：正式 worker 工件信任入口预审

2026-11-01 · 员工 C / 6.1 Terra。只读固定 main `f81e90abcb4f01901772b89ee8e0d98dfa9a5d19` / 产品 `a1af1c0169fa4aed2948b65d5e006a5b5ac51f58` / Rust `c41e4a9bbad9052444f0d9277ed5c753f42c9917`；W2 参考固定 `8937aed18124c4db75c115b9849797b49fd3c51d` / 产品 `71ca16bffb89c41e5cf5f34ed108e8b6657b6196` / Rust `58059e6805ca324d1555023d8501286fd26d9097`。没有读取 B 在途内容作为基线，没有实现或动态运行。

## 当前缺口与信任假设

main `rust-app/Cargo.toml` 是同时包含 library、`src/main.rs` Host 和 `src/bin/peachsh-wasm-worker.rs` 的包；worker 依赖同一 `peachsh` library。`build.ps1:75-88` 的 `cargo build --release --locked` 默认构建所选包的库和全部二进制，但只复制 `peachsh.exe`；没有固定 worker 副本、摘要或两者关联。Cargo 官方 [cargo build](https://doc.rust-lang.org/cargo/commands/cargo-build.html) 的 target-selection 正文明确默认构建规则，以及 `--bin` 可显式选择目标。

W2 `wasm_worker_process.rs:64-87` 的 `TrustedWorker::from_host_install(path,digest)`、`WorkerService::from_trusted` 均是 crate-private，注释也承认安装边界尚未接入；`Image::pin:459-535` 按传入摘要检查固定路径、重解析点、链接数、文件身份和打开共享方式，`verify_process` 再核进程映像。这些检查证明候选与**给定摘要**一致，不解释摘要是否可信。现有测试的环境变量路径/自报 hash 仅是测试 fixture，不能照搬为正式输入。生产尚无 worker 组装或选择，不因 W01 成功而声称已具备。

可信起点限定为用户认可的 Windows OS、Host 可执行及受信构建/安装动作；普通 WASM、插件清单、HTTP/CLI 业务参数和项目文件不可信。用户选择并安装自己的整套程序是信任起点；运行期工件校验防止工作区内容、普通请求或偶发替换变成执行对象。不新增抵抗恶意管理员、替换 Host 本身、OS/DB 回滚或第三方签名平台的要求。若安装目录与未受信工作区共用写入能力，或同用户不受控 native 代码可任意更改 Host 和安装目录，就超出该最小边界；至少应将固定安装包目录与可由 Host 工具/插件写的工作区分开，记录本用户实际文件权限和加载依赖假设，而不是假装哈希可以保护已被替换的 Host。

## 建议：同包固定文件 + Host 入口编译期摘要

**选择：**Host 编译入口 `src/main.rs` 使用 `option_env!("NHC_PACKAGED_WORKER_SHA256")` 等明确专用的编译期输入保存 worker 的 32 字节摘要（严格 64 hex 解析），并由受信 bootstrap 调用将摘要传入 library；library 自行从受信 Host 的安装位置加固定文件名 `peachsh-wasm-worker.exe` 构造路径，再经 W2 `Image::pin` 验证。编译期嵌入只在 **Host binary 源码**；不得放入共享 `src/lib.rs`、worker 源码或让共享库/worker 的构建脚本读取该摘要，否则 worker 依赖自己的 hash、每次构建改变下一次输入，形成自依赖循环。摘要不从同目录可写 `.json`、插件 manifest、项目、HTTP 或运行时环境加载；旁边一个自算 hash 文件没有认证意义。

Rust [option_env!](https://doc.rust-lang.org/std/macro.option_env.html) 明确读取编译时环境；Cargo [build-script change detection](https://doc.rust-lang.org/cargo/reference/build-scripts.html) 说明引用 `env!`/`option_env!` 的环境变化会触发相关重建。该方案的**无循环是构建图推论**，仍须以实际构建和文件 hash 实验验证：先构建 worker A；只为 Host 目标注入 A 的 hash；再构建 Host；验证已固定 worker 副本仍是 A。`option_env!` 为 None 或格式坏时，Host 仍可作为原 builtin 服务启动，但外部 worker 能力明确 `unavailable`，不得偷偷以当前磁盘内容自算 hash 信任或 fallback 到其他程序。

**bin/lib 桥：**`main.rs` 属于独立 binary crate，不能直接访问 library 的 `pub(crate)` 构造函数。后续真实接线时在 library 内部提供窄的受信 Host 启动组装入口，由 `main` 把内嵌摘要和固定协议/ABI 期望交给它；library 内部解析绝对安装路径并创建 `TrustedWorker`/`WorkerService`，由 `Engine`/`HostComposition` 拥有。该桥是受信 Rust Host 组装 API，不是 HTTP/CLI 业务参数；不复制第二份 process 模块到 main，也不需要为了阻止受信 Rust 嵌入者调用 bootstrap 发明签名认证。旧 `Engine::new_recovered` 等兼容入口明确默认**不启用 worker**；只有新受信 bootstrap 路径传入有效内嵌配置后才启用，现有 builtin 默认组合及旧调用保持原义。

**路径与失败：**正式 Host 基于 `std::env::current_exe()` 或 Windows 等价 API 获取实际 Host 安装目录，严格处理错误，仅拼接内置固定文件名；不使用当前工作目录、PATH、业务指定或测试环境。Rust [current_exe](https://doc.rust-lang.org/std/env/fn.current_exe.html) 明确路径可能受链接、重命名和平台行为影响，不能把它本身当认证；应核对安装根和 W2 已有非 reparse/文件句柄/相同身份检查，定位失败或不满足要求即不可用，不猜测备用目录。用户可写安装动作完成后把 Host 和 worker 同放受信版本目录，与可写工作区隔离；正常 Host 身份和 OS 被信任，目录/组件/加载依赖的实际限制按 W2 威胁与测试确认。

worker 缺失、读取失败、目录不符合 W2 pin、摘要不匹配、绑定协议/ABI 预期不一致或正常协议握手/返回验证失败时，只关闭该外部能力、返回固定脱敏的不可用/不兼容错误并保留 builtin 的独立可用性；已绑定外部的任务明确拒绝，不能静默改成 builtin；不能执行不匹配 worker、以旧副本/任意位置降级，不能把 `Outcome::Error` 当成功。W1 已有协议版本和完整身份检查，但**回显 manifest 不能单独证明二进制版本**：固定摘要把 Host 与选定 worker 字节绑定，构建来源与真实协议正负测试证明二者组合兼容；若共享输入/协议有意变化，必须配对重建，不依赖字符串宣称。实际 W2 非 Windows 保持现有 unsupported。

## 受信构建与安装顺序

1. 受信开发者使用现有 toolchain/lockfile、显式 profile/target/feature，固定本次源码快照及必要构建配置；在所有可能改写 worker 的测试/构建之后，**最后显式** `cargo build --locked --release --bin peachsh-wasm-worker`（开发也可提供相同顺序的 debug action）。立即从 Cargo `compiler-artifact.executable` 取得实际目标路径，复制为新的受控 staging 目录中的固定 worker 文件并记录源/副 SHA-256。Cargo [external tools](https://doc.rust-lang.org/cargo/reference/external-tools.html) 定义机器可读 artifact 的 `executable` 和 `fresh`；不硬编码与 `CARGO_TARGET_DIR` 冲突的 target 路径，也不通过硬链接代替独立快照（当前 W2 要求单链接）。
2. 检查所用源码快照、target triple/profile/features 在两个构建之间未变化，计算上述**副本**的 SHA-256；只在一次受信 Host 构建进程的环境设置 `NHC_PACKAGED_WORKER_SHA256`，显式 `cargo build --locked --release --bin peachsh`，使摘要只进入 Host binary。读取实际 Host artifact 后复制到同一 staging，环境随后恢复；bin 与 worker 名称固定。编译期环境是受信构建输入，不是安装后运行时设置。`cargo build` 不带目标参数、`cargo test`、后续其他 profile/feature 构建都可能重建或改写 target 的产物；不得把共享 target 中旧路径当冻结副本。要安装的两个 staging 副本构建后独立再核 worker hash=Host当时输入、Host hash/构建记录，以及源码/配置未变化。
3. 将完整 staging 作为一个新版本包安装到单独目录，所有文件就绪后再让新 Host 从该目录启动；不要覆盖正在运行目录中的半套。升级构建新的 Host+worker 配对及新摘要，旧实例结束后切换启动入口；中断时保留旧完整配对或新完整配对，不得混搭。可在可信开发安装动作中一次性切换，不要求新安装器、证书服务或管理员权限；如果目标目录需要提升权限，安装动作按本机 ACL 单独处理。测试 fixture 可继续内部受信构造独立快照，正常产品接口不接受 fixture 参数。

两阶段构建能产生可用的普通开发者路径：例如扩展 `build.ps1` 的受信 paired build action，统一建立 staging 并输出目录；直接 `cargo build`/`cargo run` 仍可构建 builtin Host，若未提供编译期配对摘要则外部能力禁用并给清晰诊断。绝不能让开发快捷路径悄然退化成“运行时读取同目录文件再信任自己的 hash”。

## 最小后续改动与验收

可另派唯一写入人改 `rust-app/src/main.rs`（仅 Host 内嵌摘要与受信启动调用）、`src/engine.rs`/`src/composition.rs`（新窄 bootstrap 和生命周期所有权）、`src/wasm_worker_process.rs`（固定路径/摘要校验入口按实际合并），`build.ps1`（显式双阶段成对 staging）及对应新测试；Cargo.toml 仅在目标声明确有必要时改，不新增依赖或安装器。具体桥形状需同 B 的 W2 合并后冻结；此处不授权修改其在途工作。

| 场景 | 必要观察 |
| --- | --- |
| 受信 paired 开发/发布构建 | 同源码和 profile 先 worker 后 Host；固定副本 hash 等于 Host 内嵌预期；只用固定同包路径，真实 W1/W2 调用成功；不能用测试 env 自报 digest 冒充生产入口 |
| 构建图和重建 | 测两组不同 worker 字节/摘要会使 Host 摘要随之更新；仅重建 Host 不把摘要注入共享库或改冻结 worker，且覆盖 profile、显式 target/CARGO_TARGET_DIR；build/test 后只信重新核 hash 的 staging |
| 缺失、错误/过期、混搭、半安装 | 缺 worker、单字节更改、Host A+worker B、缺/无效编译期摘要、旧协议及损坏 W1 请求均不启动不可信 worker或返回固定失败；builtin 和旧 Engine 默认行为保持 |
| 运行期替换与路径 | pin 前后替换、重解析/硬链接、预持写句柄/写映射等按 W2 W06 实际观察；失败关闭，不把当前 W2 尚未验证的检查描述为已证明。验证 Host 目录路径失败、工作目录变化、非 ASCII 正常路径及权限不足 |
| 安装和升级 | 中途失败无半套切换；新旧成对目录可回退；允许受信用户安装路径，但普通业务输入不能选择/覆盖 worker 或摘要；不依赖新安装器/外部签名服务 |

这些是建议验收而非通过结论。worker 及 OS 依赖的真实身份/写映射、竞争和生命周期仍由 W2/W06 证明，业务绑定、W3 恢复和完整产品仍未完成。本轮未改产品、构建或运行；下一步由经理冻结最小实现范围与唯一写入人，C 释放。
