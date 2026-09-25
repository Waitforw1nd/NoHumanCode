# 员工 A：审批 HTTP 候选安全复核 第1.1轮报告

日期：2026-09-26。任务：`A-R3-AUDIT`。执行标识：`A-R3-AUDIT-20260926-01`。实际模型：`gpt-5.6-sol`。

本轮只复核固定候选 `00a4c2bd675516ca61fac84c03734052ccb03660` 相对第1.0候选 `2af377026f83c0b8b3c6f24cf5a268bac07090ee` 的补证差异。提交仅修改 `NoManCode/rust-app/tests/approval_http.rs`；`src/server.rs` 字节未变，`git diff --check` 无输出。没有读取或修改C工作树、target、源码、档案或共享index，也没有重读已在第1.0报告覆盖的未变化范围。

结论：**第1.0报告要求的 AH09/AH11 两项证据已经闭合，差异内未发现新问题。** 结合第1.0报告，固定候选在 A-R3-AUDIT 范围内没有遗留阻断。C报告的定向14/14与五门禁属于C/经理运行证据，本轮A未重跑，最终验收仍由经理决定。

## 1. AH09审批事件内容边界

补测在已有两个含mock secret正文/命令参数的真实审批上，通过HTTP deny生成resolved，再从Store读取该任务的 `approval.requested` / `approval.resolved`：

- 精确断言 requested data 只有 `approval_id`、`preview`、`session_id`、`task_id`、`tool_call_id`、`tool_name`、`turn_id`。
- 精确断言 resolved data 只有 `approval_id`、`decided_at`、`decided_by`、`status`。
- 对每个审批事件序列化扫描，断言不含mock secret、`content` 或 `arguments`。
- 同时保留原DTO精确字段集合、禁止内部字段、preview字符上限及跨task隔离断言。

扫描明确限定审批事件，没有声称整个数据库或任务SSE不存在合法正文，符合正式契约修订后的内容边界。两个requested加一个resolved共三条的数量也与夹具前置事实一致，不是空集合上的真断言。

## 2. AH11错误契约与拒绝零变化

通用HTTP错误检查现在接收准确预期message，并按status断言固定 `code`：400/413/415为`request_failed`、403为`forbidden`、404为`not_found`、409为`conflict`、500为`internal`。每次还断言三安全头、`error == message`、retryable值，并扫描响应不含mock secret、SQL文本或损坏关系id。

补测以一张fresh pending审批记录事件总数为基线。超大body、Host错误、Origin错误、cross-site、各种malformed JSON、缺Content-Type及缺token的每一次拒绝后，都调用统一副作用检查，确认：

- 审批仍为pending；
- 任务总事件数完全不变；
- resolved、tool_start、tool_result均为0；
- 目标文件不存在。

这补足了第1.0候选中“部分拒绝发生在已approved记录后”的证明缺口。非法path、未知审批、未知task、坏Task JSON、真实busy决定、损坏审批关系和unknown重复决定也分别断言准确固定message/code/retryable。

## 3. 未知task 404的生产语义

C最终没有保留额外server helper或修改错误语义，这是正确的限定。固定实现的列表入口先调用 `Store::task`；该函数对SQLite `QueryReturnedNoRows` 使用 `.context("任务不存在")`，`read_error` 分类404后，`ApiError::into_response`读取最外层 `error.to_string()`，因此HTTP固定文案是“任务不存在”，不会输出底层“Query returned no rows”。新测试对真实HTTP精确断言该文案。

以上是静态调用链确认；员工A没有另跑该HTTP实验，不把C的14/14结果写成本人执行。

## 4. 交接

本轮没有返工项。第1.0报告记录的产品代码、AH01-AH08与AH10-AH14结论继续有效；本报告只关闭AH09/AH11证据缺口。经理应以 `00a4c2bd675516ca61fac84c03734052ccb03660` 的完整门禁日志作最终放行依据；任何后续SHA需重新绑定差异和运行事实。
