## 变更摘要

<!-- 简明说明本次 PR 解决的问题、新增能力或重构目标；源码引用请使用仓库相对路径（如 `src/cli.rs`）。 -->

## 影响面评估

- **CLI / HTTP API / `coda-protocol` 契约变化**：无 / （如有请说明）
- **SQLite Schema (`SCHEMA_VERSION`) 与数据迁移影响**：无 / （如有请说明）
- **安全、审批与凭据边界影响**：无 / （如有请说明）

## 验证结果

- [ ] `.\build.ps1 -Action fmt` 通过
- [ ] `.\build.ps1 -Action clippy` 零警告通过
- [ ] `.\build.ps1 -Action test` 全量测试通过
- [ ] 未包含本机绝对路径、测试密钥、构建产物或临时数据库文件
