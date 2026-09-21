# 🍑sh harness 三轮迭代计划与验收结果

日期：2026-09-20

## 第一轮：核心协议与持久状态

目标是先把并行 Agent 的边界固定下来，避免重构过程中出现重复启动、角色漂移和状态丢失。

- Rust Engine 负责成员身份、路由快照、依赖图、并发和恢复。
- SQLite schema 从固定版本改为可迁移版本；当前 `user_version=2`，新增幂等表。
- `POST /api/runs` 支持 1–200 字节的 `Idempotency-Key`，重复提交返回原运行组。
- `/api/health` 返回 `runtime` 和 `schema_version`，方便启动诊断。
- 保留旧版 3080/data 作为只读回退边界。

验收：10 个库测试、8 个运行时测试通过；HTTP 幂等重复提交和 v1→v2 schema 迁移回归测试通过。

## 第二轮：WASM 扩展沙箱

目标是让不同模型 Agent 可以调用可替换扩展，同时不把宿主权限泄露给扩展。

- 使用 Wasmi 执行 `peachsh.wasm.v1`。
- 固定 JSON ABI：`memory`、`alloc`、`run_json`；禁止所有 imports。
- 模块 8 MiB、线性内存 64 页（4 MiB）、fuel 默认 5,000,000、JSON 输入/输出 512 KiB。
- manifest 的 ABI、ID、能力、版本、fuel、内存页和 SHA-256 全部验证。
- Agent 工具 `run_wasm` 和 `/api/wasm/run` 共用同一个加载器；插件只能来自项目 `.peachsh/plugins`。

验收：解释器单元测试、manifest 越权测试和 Agent 工具到 WASM 的集成测试均通过。

## 第三轮：发布、性能和迁移收口

本轮把边界变成可交付程序：

- clippy `-D warnings` 和 release 构建通过。
- 启动前检查数据目录单实例锁、端口冲突、schema 版本和工作目录。
- 3090 Rust 实例保留，3080 Node 实例可回退；不覆盖旧 data。
- 真实模型测试继续显式开启，常规测试不携带 Key、不扣费。
- 通过文档明确“Rust 原生核心 + WASM 扩展”的架构取舍，避免把 WASM 当作权限边界之外的万能运行时。

验收：发布 EXE 复制到 `D:\peachsh-harness\bin\peachsh.exe`，健康检查返回 `runtime=rust`、`schema_version=2`；页面可创建任务、继续成员、绑定 New API 和查看插件。

## 后续迭代候选

1. WASM 签名和插件仓库索引，仍保持默认无 import。
2. 只读宿主能力（例如版本信息）以版本化 capability 明确授予，而不是增加隐式 import。
3. 数据库在线备份、幂等键过期清理和指标接口。
4. 旧版插件逐项迁移为 WASM，并为每个插件提供回滚版本。
