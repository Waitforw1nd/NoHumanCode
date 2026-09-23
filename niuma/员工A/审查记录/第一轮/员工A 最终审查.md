> 路径整理说明（2026-09-23）：本文件的本机路径已按用户要求改为相对表示，历史结论不变；旧哈希对应改写前内容，详见 [路径与证据规则](../../../../路径与可移植性.md)。

# 员工 A 最终审查：通过

审查日期：2026-09-23  
审查范围：Project / Session / Turn / Agent / Task 领域契约、SQLite Repository，以及现有 Engine / HTTP 的基础接入。  
代码基线：`8f0ccdc`，加本次直接修复后的当前工作区代码。  
最终结论：**APPROVED — 员工 A 第一轮基础交付通过，可以进入后续模块开发。**

本次按用户“直接帮忙编写通过，然后出最后审查”的授权，直接补齐实现和测试。通过结论针对修复后的代码，不是追认第 1.5 轮报告中的未完成内容。第 1.0～1.5 轮审查保留为历史；本文件是员工 A 本轮的最终结论。

## 1. 放行依据

当前创建入口已经形成实际链路：

```text
POST /api/runs
  → Engine 统一幂等检查
  → Store::commit_turn_bundle
  → Project / Session / Agent / Turn / TurnTask / ID 依赖
  → legacy Run / Task 兼容投影、幂等记录、初始事件
  → 同一 SQLite 事务提交
```

执行中的任务状态更新同步修改旧 Task、新 TurnTask、聚合 Turn，并写入状态事件；继续和启动恢复也走事务。只读领域 API 和 Session SSE 已接入并经过真实本机 HTTP 测试。审查范围内未发现仍需阻止员工 A 基础交付的具体问题。

## 2. 历次问题的最终处理

| 问题 | 最终实现与解决思路 | 结果 |
| --- | --- | --- |
| R1：新旧幂等语义分裂 | Store 的 replay 与提交共用 `classify_both`；Engine 先区分 Turn / LegacyRun。旧兼容查询也走统一判断。不同哈希或绑定冲突使用结构化错误，HTTP 返回 `409 / conflict`。 | 关闭 |
| R1：旧运行回放丢任务 | 按原行顺序读取完整旧任务，校验 JSON 中的 ID、run_id 与关系列一致；读取历史 Task 时执行已知凭据脱敏。不存在、无效 JSON、身份不一致明确失败。 | 关闭 |
| R2：只检查列名或索引名 | 校验七张领域表的声明类型、NOT NULL、PK 顺序、FK 映射及动作、真实唯一约束，以及命名索引的列、顺序、唯一性、排序规则和非部分索引属性。已有 schema 6 标记也复查结构。 | 关闭 |
| R2：迁移失败或半迁移 | schema 6 的 DDL、校验、标记和版本推进在事务中完成。已有 schema 6 仅安全补建缺失的 legacy task 唯一索引，损坏结构或重复数据拒绝打开。 | 关闭 |
| R3：任务引用歧义与错误事件归属 | 按真实 Task 行身份比较 canonical ID / legacy ID 命中，拒绝不同任务命中；迁移检查领域任务的历史交叉碰撞。新旧任务写入拒绝跨表身份碰撞；事件统一保存 legacy_task_id，并核对 Turn、Session 及旧 Run 归属。 | 关闭 |
| R4：输入与凭据边界不统一 | 底层写方法收紧，Store / Repository 共用标识、元数据和凭据处理。ID、幂等 key、显示文本、路径分别执行适用约束；路径不套用标题的 500 字节上限，key 保留 HTTP 约定的 200 字节上限。 | 关闭 |
| R4：凭据识别与测试失真 | 修复缩写字段识别，使用独立普通值验证各敏感字段，覆盖文本中间的已知 token。最后审查发现的 `Authorization: Bearer …` 漏脱敏也已修复并加入回归。 | 关闭 |
| 依赖替换与吞错后提交 | 依赖替换收为内部方法；存在性、同 Turn、自依赖、重复及循环检查封装在 SAVEPOINT 中。完整 Turn 提交也使用 SAVEPOINT，失败后即使外层继续提交，也不会保留局部修改。 | 关闭 |
| R5：组合创建与状态更新不原子 | 新增组合命令，完整新旧任务投影必须一一对应且初始状态一致。状态、投影、事件同事务；事件失败回滚任务和 Turn。非法状态倒退被拒绝，interrupted 重新执行需要显式 resume。 | 基础范围关闭 |
| R6：新领域没有进入产品写路径 | Engine 新创建改为领域组合提交；新任务调度读取稳定 ID 依赖，只有旧数据保留名称兼容。新增领域查询与 Session SSE；核心事件 kind 受声明列表约束，扩展未知事件有显式受限命名。 | 基础范围关闭 |

反复出现的问题这次采用边界收口解决：幂等集中判断，状态与事件集中提交，依赖修改封装事务，事件身份只做一次明确解析。后续模块应复用这些入口，避免再次各自实现一套判断。

## 3. 关键代码与新增验收

- [Store 组合提交、状态事务与恢复](../../../../NoManCode/rust-app/src/store.rs)：`commit_turn_bundle`、`save_task`、`resume_task`、`recover`、`classify_both`。
- [Repository 迁移结构与事件归属](../../../../NoManCode/rust-app/src/repository.rs)：`verify_current_schema`、schema 元数据、`resolve_task_turn`、`insert_event_tx`、`legacy_run`。
- [生命周期转换](../../../../NoManCode/rust-app/crates/protocol/src/lib.rs) 与 [事件 kind 约束](../../../../NoManCode/rust-app/src/domain.rs)。
- [Engine 接入](../../../../NoManCode/rust-app/src/engine.rs) 与 [HTTP 查询和冲突映射](../../../../NoManCode/rust-app/src/server.rs)。
- [凭据处理与回归](../../../../NoManCode/rust-app/src/secrets.rs)。
- [最终验收测试](../../../../NoManCode/rust-app/tests/final_acceptance.rs)：组合成功、无效依赖/缺失投影回滚、幂等重试无新增对象、完整旧数据回放、状态同步、非法转换、显式继续、伪造事件拒绝，以及 SQLite trigger 注入事件失败后的提交/恢复/继续回滚。
- [运行与 HTTP 集成测试](../../../../NoManCode/rust-app/tests/runtime.rs)：使用本机模拟 Provider，验证创建、领域查询、Run/Session SSE 与游标、同请求回放及不同请求 `409`，确认没有重复模型调用。

额外交叉审查已确认：初始 Turn/Task 状态不一致、interrupted 直接入队、Authorization 凭据残留三个问题均已闭合。

## 4. 实际执行的最终检查

执行目录：`./NoManCode\rust-app`。使用项目 `build.ps1` 配置本机 MSVC / Windows SDK 环境。

| 检查 | 命令 | 实际结果 |
| --- | --- | --- |
| 工作区测试 | `pwsh -NoProfile -File build.ps1 -Action test` | 71 通过，0 失败，1 忽略；退出码 0 |
| 格式 | `pwsh -NoProfile -File build.ps1 -Action fmt` | 通过；退出码 0 |
| Clippy | `pwsh -NoProfile -File build.ps1 -Action clippy` | workspace / all-targets / `-D warnings` 通过；退出码 0 |
| WASM 编译检查 | `pwsh -NoProfile -File build.ps1 -Action wasm-check` | 通过；退出码 0 |
| 补丁空白检查 | `git diff --check` | 通过；退出码 0 |

71 项通过的组成：43 个库测试、4 个 adversarial 测试、9 个 final_acceptance 测试、10 个 runtime 测试、5 个 protocol 测试。

忽略的是现有 `xpeach_native_stream_and_coding_tool` 在线测试，需要 `PEACHSH_TEST_KEY` 并产生付费模型调用。本次没有执行该在线测试，HTTP 集成验收使用本机模拟服务。工具仍报告 `proc-macro-error2 2.0.1` 的未来 Rust 兼容提示；这不是本次编译或 Clippy 失败。

## 5. 后续分工与验收边界

员工 A 的基础契约可供后续员工使用。后续开发必须遵守：

1. 创建和执行接入统一 Store 命令，复用稳定 ID；不要从 UI 或业务模块自行写 SQLite 表。
2. 状态变化连同事件提交；凭据进入 DPAPI，业务数据经过现有持久化边界。
3. 新增事件先扩展声明契约和兼容测试；恢复、继续、幂等重试不得重复派发已回放的任务。
4. 后续实现 Approval / ToolCall、持久 Scheduler、Provider / KeyPool、Workspace / Checkpoint、插件运行时和 CLI 时，各自提交独立验收证据。

本次通过的是员工 A 的基础交付。项目书中的逐次审批、完整 CLI/插件生命周期、Diff/Checkpoint、持久调度、多 Key 健康池及完整连续会话产品流程仍属于后续模块；状态枚举或读取 API 的存在不代表这些业务能力已经完成。现有 resume 保留 Task 和模型身份，尚不等同于完整的多 Turn 对话产品设计。

已知凭据格式和敏感字段的脱敏属于防御措施，不承诺识别任意自然语言中的未知秘密。旧数据按兼容路径读取，也没有在本轮被全量改造为新的会话产品模型。

本次修改保存在工作区，未创建 Git 提交、未发布或替换运行中的程序。

**最终决定：员工 A 第一轮通过；解除此前对其领域/Repository 基础依赖的阻塞，允许后续模块按项目书继续开发。**
