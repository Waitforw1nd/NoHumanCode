# 员工D review 1.0：插件 Host 注册表与可撤销生命周期（D-R2-01 / NEXT-02B）

日期：2026-09-25；审查人：项目经理；对象：候选提交 `1223f6b80591eb5c54e480f09991f67e525e3b08`（分支 `codex/d/plugin-host`，基线 `b67fedf`）与 [第1.0轮报告](<../../提交报告/第二轮/员工D（插件Host注册表与生命周期 第1.0轮报告）.md>)。结论：**APPROVED（契约范围：builtin 插件 Host 注册表 + 可撤销生命周期）**。

## 1. 候选与范围核对

- `git -C ../nhc-d-plugin-host rev-parse HEAD` = `1223f6b80591eb5c54e480f09991f67e525e3b08`，与报告一致；`git diff b67fedf..HEAD` 恰好三处：新 `src/plugin_host.rs`（1488 行）、新 `tests/plugin_host.rs`（2323 行）、`src/lib.rs` 仅 `+pub mod plugin_host;`。
- 三文件 sha256 与报告/证据 `candidate-sha256.txt` 逐一吻合；工作树 `status` 干净；`plugin_catalog.rs`/`wasm.rs`/`server.rs`/Cargo/锁文件/旧测试零改动。集成测试仅经 `peachsh::plugin_catalog`/`peachsh::plugin_host` 公开路径调用，无私有成员访问。
- 证据目录齐备：8 步门禁 logs、gate-results.json、run-gate.ps1、msvc-env.ps1；脚本/JSON/txt 检索无本机绝对路径（R1 的 P2 路径问题已闭合）。

## 2. 经理独立实跑（同一候选）

工作树 `NoManCode/rust-app`，本机 PS 5.1 shim + 显式 MSVC/SDK 变量环境（首次直跑 bash 遇 MSYS2 环境转换致 `LNK1181 bcrypt.lib`，`MSYS2_ENV_CONV_EXCL='LIB;INCLUDE'` 后全绿——环境适配问题，非源码缺陷）：

| 步骤 | 结果 |
| --- | --- |
| `cargo test -p peachsh --locked --test plugin_host` | **23 通过 / 0 失败** |
| `cargo test -p peachsh --locked --lib plugin_host::` | **6 通过** |
| `cargo clippy -p peachsh --lib --test plugin_host --locked -- -D warnings` | 0 告警 |
| `cargo fmt --all -- --check` | 无 diff |
| `cargo check -p peachsh-ui --target wasm32-unknown-unknown --locked` | 通过 |
| `cargo test --workspace --locked` | **172 通过 / 0 失败 / 1 付费忽略**（lib 59 含本模块 6、plugin_catalog 17、plugin_host 23、adversarial 4、final 9、http 11、runtime 10、session_turns 17、turn_http 17、protocol 5） |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 告警 |

## 3. 契约逐项复核（§3）

- §3.1：`PluginHost::new` 一 scope 一 catalog；`catalog`/`catalog_mut` 只读/可变访问；`resolve` 委托；`start` 当场 resolve 并预扫 roots（Active/Failed root → `InvalidState`）。✓
- §3.2：`BuiltinPlugin: Send` activate/deactivate；`with_factory`；wasm/process → `UnsupportedRuntime{unwound}`；builtin 无工厂 → `MissingFactory{unwound}`；`catch_unwind` 隔离；每次 Loaded→Active 工厂取新对象。✓
- §3.3：`bound` 限 manifest.requires+计划绑定、返回 `BoundInterface{key,provider_id,payload:&dyn Any}`；`register_effect` 限 provides、`DuplicateEffect`；`unregister_effect` 限自身；`subscribe` 限 requires+绑定+event kind 且记录 provider_id 防串台；`emit` 限自身 provides+event kind、按订阅者 id 升序、不补投；activate 结束 provides 全覆盖校验 `ProvidesNotCovered{missing,unwound}`。✓
- §3.4：`EffectRecord`/`instances()`/`effects()`/`pending_recovery()` 快照确定序。✓
- §3.5：五态+`Failed{stage}`；`stop` 有 Active 消费者 → `DependentsActive`（id 序）；`stop_subtree` 经激活期 bindings 传递闭包、按激活逆序级联；`unload` 拒绝 Active、Failed 残留转恢复清单；非法迁移 `InvalidState`、未知 `UnknownPlugin`。✓
- §3.6：`HostError` 变体全族在案（含补充 `UnknownEffect`）；`start` 失败回滚本批列 `unwound`；deactivate 失败 → `Failed{Deactivate}`+恢复清单且注册表无残留。✓
- §3.7：BTreeMap/排序遍历、无 IO、display_name 非身份。✓
- §4 验收：H01–H12 均有真实测试一一对应，另补 11 个契约规则反例（订阅回调 panic 归因、绑定 provider 防串台、Failed 成员拒绝、restart 取新对象、级联多处失败登记、回滚期 deactivate 失败、remove_descriptor 非卸载、emit 边界等），断言均与语义对应，非空转。

## 4. 契约反馈处理（报告 §契约反馈 1–5）

1. `HostError::PluginPanic` 无 `unwound` 字段——契约字面形状如此；回滚实际执行但名单不透出。**列 P2**：后续修订统一 start 失败证据（PluginStartError cause=Panic 带 unwound，或给 PluginPanic 补 unwound）。
2. `PluginFailureCause` 裁至三个可达变体——合理，错误分类仍 typed。
3. `FailureStage::Deliver` 新增标记订阅回调 panic 归属——语义清晰，接受。
4. 订阅记录 provider_id、emit 按 emitter 过滤——超出契约最低要求的正确性加强，接受。
5. `emit` 返回成功投递数（panic 订阅者不计）——与"不补投、故障隔离"一致，接受。

## 5. 非阻断观察（P2，不要求本轮返工）

- `stop_subtree` 级联中遇 deactivate 失败返回首个错误，已停列表不随错误透出（恢复清单有登记）。
- `unload` 对 `Failed{Activate}` 残留 effect 的防御性恢复登记在当前实现不可达（teardown 恒回收），属无害防御。
- 订阅回调 panic 的实例 Failed{Deliver} 且不级联其依赖者——已记录并测试的取舍；后续契约可明确依赖者后续 `bound` 命中 `UnboundInterface` 的预期。
- `git worktree` 检出于 `b67fedf`（不含其后 docs 提交），与登记基线一致。

## 6. 结论与下一步

APPROVED。按 Git 规范 §6 由经理串行整合：`main` 与该分支已分叉（main 有基线后 docs 提交），采用 merge commit 保留受审 SHA `1223f6b`；整合后按 §6 核对清理 `../nhc-d-plugin-host` 工作树与 `codex/d/plugin-host` 分支。后续切片：审批/capability、wasm/process 运行时接入、持久化、Workspace/CLI。整合记录另写 `niuma/项目经理/审查记录/2026-09-25NEXT-02B整合记录.md`。
