# 员工A（审批 CLI 修复候选复核 第1.3轮报告）

## 对象与边界

- 原阻断候选：`5bc5e0f4128b512009cb46e3014076f7d0069aeb`。
- 生产修复候选：`fd3e7a373466e4b7628196faaf0974e66b461559`，仅改 `src/cli.rs` 与 `tests/cli.rs`。
- 最终补证候选：`0d4dc9af863458e3d6fe2ec7c8b380eebaaafdcc`，相对 `fd3e7a3` 仅在 `tests/cli.rs` 新增 5 行 instance-lock 精确错误断言，生产代码不变。
- 执行者：员工 A；实际模型 `gpt-5.6-sol`。本轮仅 `git show`/`git diff`/`git diff --check`；未改 C 源码、target、index，未运行测试或五门禁。

## 差异结论

C-F1 已闭合：每个命令显式携带 `SuccessShape::List/Single`，成功响应不再按服务端是否含 `approvals` 字段选择形状；新增真实二进制反形状测试覆盖 list/get/approve/deny，decision POST 计数保持一次。

C-F2 已闭合：decision 的 decoded JSON 在 typed 反序列化和未知字段丢弃前递归扫描所有字符串值及对象键中的已知 bootstrap token，不再依赖 JSON 重序列化文本。测试使用含双引号和反斜杠的 token，覆盖成功白名单字段、人读/JSON模式及错误额外嵌套字段，验证 token 不输出且 POST 不重试。

第1.1报告的补证项也已处理：CL01 增加列表人读、单卡 JSON 和双向形状拒绝；CL03 使用隔离 temp parent/cwd；CL04 对 task 404、approval 404、重复决定 409 精确断言结构；CL05 将 dot/dotdot、控制字符、129字节与参数冲突拆开，并捕获合法128字节和特殊字符编码后的目标 URI。

`0d4dc9a` 新增的固定 instance-lock 文案断言关闭了 A 对相邻端口偶然占用可能造成 CL08 假阳性的最后建议：第二服务必须因同一 data-dir 的 instance lock 失败，随后 CLI 仍能连接第一服务读取审批。该差异没有改变生产行为，`git diff --check` 无输出。

## 结论与限制

静态差异范围内未发现新增生产阻断，原 C-F1/F2 与 A 第1.1所列 CL01/03/04/05 证据缺口均已闭合。可进入经理对 `0d4dc9a` 的定向、fmt、clippy 与完整五门禁判定。

A 没有执行 C 报告的 11/11 或任何门禁；经理应以固定 SHA 的真实日志决定最终验收。若门禁通过且 SHA 不再变化，A 无需重读完整 CLI。
