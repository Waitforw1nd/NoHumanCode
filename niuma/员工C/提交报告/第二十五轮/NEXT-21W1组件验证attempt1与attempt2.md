# NEXT-21W1 组件验证 attempt1 与 attempt2

2026-11-01 · 员工 C / 6.1 Terra。经理签发唯一动态窗口；产品独立树 `codex/c/wasm-worker-core`，主仓仅写本人报告与身份。

## attempt1：首失败停止

受测 `fe226ea80391fb7d3aa9caefe7d365bf3c1d53b4` / 产品 `24ea0639a2c8464013488c93f771d0d45dfeb192` / Rust `4826aacc60e90745aa76d818ee315e4e64b31773`。按审查意见补全六个失败场景的 Module/Execution 精确类别、start 循环耗尽 fuel、worker 显式 flush 失败非零。workspace all-targets check、worker bin build 均 exit 0；测试 exit 101，4 passed / 1 failed；fmt 与 Clippy 未执行。失败测试 `decision_validation_and_strict_json` 证明内部标签的单位变体 `finish` 即使配置 deny_unknown_fields 仍接受额外 `secret`；不能称协议严格通过。受测 worker 构建 SHA-256 `ce7aa741e42fa279113d638864b431fa0581830155e548fd9ffbf7008d2e0b63`，未运行此 bin。现场在独立树 `NoManCode/rust-app/.local/next21w1-c-fe226ea8-attempt1/`，保留原日志、清单、tmp；target 被后续 attempt2 复用，因此当前 target 不再是 attempt1 二进制原件，此哈希是当时记录，不宣称二进制副本仍未变化。

## 修复与 attempt2

按经理裁决在授权路径内将 `Phase::AfterNoTools`、`Decision::Finish/Stop` 改为空结构体变体，保持线上 JSON 形状，加入从实际分帧请求/响应入口拒绝额外字段及合法 finish/stop 的测试；不放宽原断言。修复提交 `7229dae2d6e8557a548f0ff5444c492243d901c6`，产品树 `7267b1187c5e46f6632cf546bc6ab8638d33a3b9`、Rust 树 `fb9828e63c4f11c5633c83e570b761de6dfc438d`。

在此固定身份、独立树 clean、index/unmerged 空下，E 盘 target/tmp，offline/locked、jobs=2、单测试线程，从头执行，顺序与退出码如下：

1. `cargo check --offline --locked --workspace --all-targets`：0。
2. `cargo build --offline --locked --bin peachsh-wasm-worker`：0；新 worker SHA-256 `cd3e6c5514fee44d4786209ba05e0f30829df96b20230d052f336aa49775c94b`，仅构建并取 hash，未启动该 bin。
3. `cargo test --offline --locked --test wasm_worker_core -- --test-threads=1`：0，6 passed / 0 failed / 0 ignored。
4. `cargo fmt --all -- --check`：0。
5. `cargo clippy --offline --locked --workspace --all-targets -- -D warnings`：0。

独立树 `NoManCode/rust-app/.local/next21w1-c-7229dae2-attempt2/` 保留 runner、各步 stdout/stderr/命令及退出码、环境、pre/postflight 和源码清单；其 target 复用 attempt1 的 E 盘 target，build 后重新记录上述新 hash。前置、每步后与最终 7 份源码清单 SHA-256 均为 `ba8b2dca55e884f5f52cc344bffc08b22ceb9ce2bada871e73838dd33d080260`，结束 HEAD/双树未变，status/index/unmerged 空，未见重型进程。来源清单是工作区文件哈希和 tracked 路径清单，并非另行逐文件计算 Git blob 的证明。

通过场景包括：有实际 start 的 WAT、before/after 合法策略、错误签名/import/无 memory maximum/超限 table 为 Module、坏指针与 run/start 耗尽 fuel 为 Execution、错误策略为 Decision；边界帧超长前置拒绝、截断、尾随、错误关联/版本、非法 UTF-8/JSON、未知字段、索引/投票数与阶段。start 测试证明单实例内正确状态；跨此前销毁实例的累计次数仍仅有单个实例化调用的静态代码证据。

本组件通过不证明真实 worker 子进程通信、进程隔离、Job/LPAC、总期限、取消与 Host 强杀回收，也不证明 Engine/PluginHost 真实绑定、持久选择和恢复。没有启动 worker bin、执行旧全量故事或改 B 封装。下一步等待经理复核和 W2 另行派发。

## 产物留存补正（经理复核后，2026-11-01）

上文 attempt2 第 2 步 `cd3e6c5514fee44d4786209ba05e0f30829df96b20230d052f336aa49775c94b` 是显式 build **当时** `worker-binary.json` 所记哈希；当时未复制独立冻结副本，且未运行该 bin。当前共享 target 的 `debug/peachsh-wasm-worker.exe` 实核 SHA-256 为 `805a8e712dcf62f007992d1de1f1a357611535342053bc0ad987f9d540791fdd`，大小 10852864 字节，修改时间 2026-11-01 08:23:14.9686293 +08:00（00:23:14.9686293 UTC）。不能把当前文件当成 cd3 对应的原始构建副本，也不能仅凭当前文件重新验证当时哈希。

原日志显示显式 build 结束 00:23:02.267 UTC，随后组件 test 00:23:02.802 至 00:23:19.935 UTC，日志有 `Compiling peachsh`，bin 修改时间落在此区间；测试日志只明确列出运行 `wasm_worker_core` 测试程序。只读核对 `debug/deps/peachsh_wasm_worker.exe` 目前也为 805a，相关 `peachsh-79b7aa3736c9edba/bin-peachsh-wasm-worker.json` fingerprint 与依赖记录的修改时间亦位于 test 区间。它们支持共享 target 内 worker 产物在后续阶段发生变化的判断，但日志没有保留逐条 rustc/link 调用或每阶段独立副本，**不足以唯一确定覆盖动作、原因或当前文件完整构建来源**；不把时间相关性写成已证因果。

此产物留存限制不自动否定已有 6 项组件测试和五步命令记录，仍不证明 bin 已运行。W2 在最终需启动前，应在其他可能改写共享 target 的步骤之后显式构建，将产物复制到独立固定位置并记录源/副哈希、来源和时间，再对该副本实施受控启动验证。本次补正只读原日志、fingerprint 与哈希，未重跑构建或测试，旧记录保留。
