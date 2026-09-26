# 员工C（Workspace W13 HTTP补证 第1.0轮报告）

2026-09-26，`C-R5-CRASH-SUPPORT` 补充1，同一执行者，实际模型 GPT-6-sol。经理补充任务后在干净支持分支同步 B 固定输入 `74229736ffca4f712a7564b3084110c51c58ea44`；C 仅新增 `NoManCode/rust-app/tests/workspace_http.rs`。初稿提交 `992a9cb60cad7ab212fd7a855efa5f937e835a5e`，经理复核后补强最终提交 `cf1a9c2c581985d1ce8a8907f04898df7c46c99f`，文件 SHA-256 `44EAC388D85C4AE8D2D119671AF0887A512E269A3260C61887D3CFE34A5F3D8E`。独立工作树 `../nhc-c-workspace-crash/`、分支 `codex/c/workspace-crash-tests`，提交后干净。C 未修改 B 实现、旧测试、Cargo、脚本或共享暂存区；交付不等于经理验收。

## 真实 HTTP 结果

- 正常场景：本机 mock Provider 真实批准并写入文件，GET changes 核 typed 相对 path、kind、state、摘要与 restorable；POST restore 核 200、complete receipt、稳定 restore_id、逐路径 complete、实际 before 字节精确恢复；恢复后 GET 查询历史 receipt。空任务 POST 为 complete/0 且无 operation，未知 task GET/POST 均 404，非法 task ID GET/POST 均 400。
- 拒绝与秘密：GET 非法 Host/Origin，POST 缺/错 token、非法 Host/Origin 均 403 且有安全响应头；每个拒绝后核 DB change/restore 行数与文件字节未变。自真实 DB `before_blob` 取密文字节，正常 changes、restore、恢复后查询、相关持久事件及真实 SSE 帧都扫描 before 明文、密文字节/十六进制、内部绝对工作区路径及 JSON 转义路径，未出现。
- unknown 与 500：测试 DB 触发器让恢复 outcome 提交失败，真实 POST 返回 409 safe unknown receipt、逐路径 unknown 与稳定 ID；重复 POST 409 同 ID、不再次写。之后在临时 DB 重命名 change 表，真实 GET 返回固定 500 `internal`/固定安全 message/`retryable=false`，不含 SQL 私有标记、before 或绝对路径。
- partial：两路径真实写入后，临时 DB 触发器在第一路径 outcome 完成提交前提供确定性窗口；父测试确认第一文件已还原且 SQLite 写事务占用，随后 Windows 独占打开第二路径。真实 POST 在第二路径文件副作用前复查失败，409 receipt 为 `partial`、`restored=1`、第一 outcome complete、第二 outcome unknown；第二文件仍是任务产物。释放锁后重复 POST 返回同 receipt 且不写第二文件。这证明执行中部分完成，不声称第二路径曾开始文件系统修改。

定向 `workspace_http` 3 passed/0 failed；`cargo fmt --all -- --check`、`cargo clippy --test workspace_http --locked -- -D warnings` 均 exit 0。原始/发布日志、命令时间、退出码及前后 SHA-256 在[证据清单](C-R5-HTTP证据/清单.md)。首次未给 `Start-Process` 子进程继承完整 MSVC 环境 exit 101，修正后重跑；开发中 500 夹具直接 DROP 表触发 FK 失败、partial 单线程阻塞，以及 Clippy 测试代码一条不必要分配均已修复，不能把这些早期运行称作通过。C 未运行全量五门禁或付费 live。

本文件补的是 W13 路由矩阵和 W06 执行期 partial 子项；完整 W01-W14、全量组合门禁与产品验收仍由 B/经理协调。历史四个 crash 场景和已验收 CLI 保持原事实，不在本轮重跑或重开。
