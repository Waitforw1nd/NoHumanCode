# 员工D：builtin工具生产装配 第1.0轮报告

任务 D-R3-01 / NEXT-05 v1.0；执行标识 `D-R3-01-20260927-000811`；实际模型 GPT-6-astra。提交状态：候选冻结，待经理 review/整合，不自行宣称已验收。

## 接收、基线与继承

经理正式派发登记为 2026-09-27 00:08:11（UTC+8），本执行者在本对话实际接收并回执后开工；实际回执秒值未另采样，不能将派发时间冒充接收时钟。先前只读设计支持无产品/档案写入或测试。本轮完整读取 [冻结契约](../../../项目经理/任务/2026-09-27NEXT-05单Agent贯通契约.md)，基线 `5cf74319ea3b37af9c27821a6eaac944241473e4`，独立树 `../nhc-d-tool-runtime`，分支 `codex/d/tool-runtime`，开工干净。共享树原有 3 修改、6 未跟踪档案保持，不动共享 index。

服务中断后按用户“继续”与经理要求接续同一执行标识，未换模型/重开任务。读取经理 `./.local/review-evidence/NEXT05-service-20260927-003608/manifest.json`，核对 D 五处在途 SHA-256 全部匹配，HEAD 仍为原基线；未覆盖/清理在途。

候选提交：`3d00f121b0c66124cbb2afc2d6fdf0d1cf504c10`。本人仅提交独立工作树五个授权源码文件，提交后 `git status --short` 空，明确干净冻结；经理是整合人与共享文档提交执行人。

## 交付与公共契约

- `src/plugin_host.rs`：增加 `ResolvedPayload<T>`、`resolve_bound<T: Any + Clone>` 与 `HostError::PayloadTypeMismatch`。从消费者激活 manifest/plan 找绑定，验证双方 Active，再从该提供者 live effect downcast/clone；不按目录当前状态重解析，不全局找同名服务。
- `src/tool_runtime.rs`：Host 私有 Mutex、两个 manifest 与 factories、真实 service payload；`builtin/definitions/acquire/disable_builtin_files/unload_builtin_files` 与冻结 API 一致。`ToolLease` 不可 Clone，执行为 `pub(crate)` 且消耗 self；不公开 raw Arc/registry。另绑定 task.id/workspace/spec，拒绝后续错投任务或权限变化。
- `src/lib.rs`：仅新增一行模块导出。
- `tests/plugin_host.rs` 与 `tests/tool_runtime.rs`：resolver 定向反例、权限上限与兼容分支回流拒绝；模块内测试验证 private service 实际执行与 admission 屏障。

文件工具由 `builtin.files` 提供 `service/tool.read_file/1.0.0`、`service/tool.write_file/1.0.0`，`builtin.engine-tools` 声明 requires 并真实绑定。payload 为私有 `Arc<dyn ToolServiceV1 + Send + Sync>`，definition/execute 均经该 payload；生产服务执行复用原 workspace 实现，无 Store/Key。manifest 权限只是声明，Host 任务权限仍是上限。list/search/run_command/run_wasm 明确为兼容分支，不宣称已插件化。

Engine 接入不在 D 所有权内，候选本身仍需 B 接入生产 Engine。`execute` 的局部 `allow(dead_code)` 仅为独立 D 候选尚无 Engine 生产 caller；模块测试已实际调用，未屏蔽其他 lint。没有 schema/migration/dependency/HTTP DTO/凭据存储变化。

## 验收条目与实测边界

| 条目 | 实现/证据 | 状态与范围 |
| --- | --- | --- |
| N08 真实绑定/payload | `n08_host_typed_resolver_obeys_live_activation_binding_and_payload_type`：Arc 指针、effect id、实际 counter 调用；错类型、错版本、undeclared、Stopped、独立 Host 无 consumer 拒绝；移除 descriptor 不改活跃快照 | 定向实跑通过；提供者非Active检查有代码，未单独伪造 Host 内部不变量触发此分支 |
| N08 非 bool 装配 | 模块 `n08_definition_and_execution_use_actual_registry_payload`：替代服务提供 marker schema 与返回值，执行计数一次 | 实跑通过，不能由旧 workspace 分发产生该结果 |
| N09 停止先胜 | `n09_stop_first_rejects_admission_and_unload_leaves_no_effects`：线程屏障，停止后调用回调不能进入，卸载无 effect | 实跑通过；模块 admission 回调，不冒称真实 Store claim |
| N09 admission先胜 | `n09_admit_first_can_finish_after_stop_without_retaining_registry_effects`：回调持锁屏障，stop 阻塞，释放后撤销 effect，原 lease 完成且计数一次，后续调用不admit | 实跑通过；无锁跨 await，Engine FS 竞态由 B 补证 |
| N09 claim失败 | `n09_claim_failure_returns_no_lease_and_releases_lock_for_stop`：失败回调与stop竞争，原错误保留，无payload执行、无lease、无effect残留 | 实跑通过；真实数据库 claim 失败由 B 补证 |
| 任务权限与兼容 | 集成 `definitions_and_admission_obey_task_permission_ceiling`、`revoked_file_tools_never_reappear_through_compatibility_dispatch` | 实跑通过；tools关闭/无scope/未授权command拒绝，新runtime重建仅对象级证据，不是N12进程重启 |
| 文件与lease约束 | 模块实际write/read临时普通文件、拒绝父目录路径，lease执行拒绝已变更spec | 实跑通过；模块直接调用仅测试私有接口，生产必须在B原审批/perform_tool中调用 |
| N14局部回归 | 既有plugin_host 23项加新增1项共24项通过；check/fmt/clippy通过 | 最终组合全workspace test/wasm-check由经理串行安排，D未重复全量 |

失败/取消/恢复口径：审批等待时不得持 lease；acquire 锁内先验证服务/权限并完成 lease 快照，再同步 admit。停止先胜无 admission，admit先胜可以用既有结果事务完成；不强杀在途副作用。回调不得 re-enter runtime/await/执行FS，不允许 Store→runtime 反向锁序。lease不持久化、不恢复；Engine claim/取消/unknown/Workspace恢复安全规则由B保持，本模块无自动重试。

## 实际命令与结果

Rust命令工作目录均为独立树 `NoManCode/rust-app`，使用仓库 `rust-toolchain.toml` 的 stable、`build.ps1` 配置 MSVC/SDK，进程级独立 `CARGO_TARGET_DIR=../nhc-d-tool-runtime/.local/target`（相对仓库根表示），无全局环境变更。原始日志保存在独立树 `.local/d-r3-evidence`；[证据清单](D-R3-01证据/manifest.json)记录原始/发布副本哈希，发布日志只转换编码与相对化工作树前缀。

| 实跑命令 | 退出码 / 结果 | 证据 |
| --- | --- | --- |
| 根目录 `cargo fmt --manifest-path NoManCode/rust-app/Cargo.toml --all`（首次） | 1；根目录默认Cargo 1.82不支持edition2024；未格式化成功，不是产品失败 | 本轮工具输出；未留独立原始日志 |
| 在Rust目录 `cargo fmt --all` | 0；使用仓库stable修正格式 | 本轮工具输出 |
| `pwsh -NoProfile -File build.ps1 -Action check`；后续同命令dot-source保留构建环境 | 0；首次及最终定向输入检查均通过 | [check.log](D-R3-01证据/check.log) |
| `cargo test --locked --lib tool_runtime::tests -- --nocapture` | 0；6 passed / 0 failed | [runtime-unit.log](D-R3-01证据/runtime-unit.log) |
| `cargo test --locked --test tool_runtime --test plugin_host` | 0；24 + 2 passed / 0 failed | [integration.log](D-R3-01证据/integration.log) |
| `pwsh -NoProfile -File build.ps1 -Action fmt` | 0 | [fmt.log](D-R3-01证据/fmt.log) |
| `pwsh -NoProfile -File build.ps1 -Action clippy` | 0；workspace/all-targets，-D warnings | [clippy.log](D-R3-01证据/clippy.log) |
| `git diff --check` | 0 | 本轮工具输出 |
| `git commit` 与提交后 `git status --short` | 0；候选固定，输出空 | 候选SHA及Git记录 |

Cargo显示上游 `proc-macro-error2 v2.0.1` future-incompat提示；不是本轮新增依赖，现行clippy退出0。不运行release、付费live、推送；本轮未运行全workspace test/wasm-check、真实CLI/Host重启，不能据此写N01/N02/N12通过。

## 交接

经理可审查固定候选并合入 B，B 继续唯一 Engine 接入；D 等待 review，不改冻结源码。N09真实Store/Engine、N12真进程重启和最终联合门禁未完成，是分层交付中的明确依赖，不以D模块测试替代。报告和身份均写共享员工D目录，工作树内 niuma 副本未修改。
