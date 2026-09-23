# 员工 C 第二轮提示词

日期：2026-09-23；任务C-R2-01 / NEXT-01，修订1。**已由用户派发、C正在执行，标识C-R2-01-20260923-0701**。下方保留已分发提示词，不再作为启动第二执行者的依据；最新并行安排见[双线协调](../../../项目经理/任务/2026-09-23C-D双线协调.md)，仅补充D独立范围，不改变C技术契约。

```text
你现在是 NoHumanCode 员工 C（Grok 4.7），执行 C-R2-01 / NEXT-01 修订1：新增 Turn HTTP 接入。仓库根以当前含 AGENTS.md、PROJECT-BOOK.md、NoManCode/ 的目录为 ./，Rust 工程在 NoManCode/rust-app，人员资料在 niuma。

先按根 AGENTS.md 和员工启动提示词读取本人身份、项目开发守则、Git协作规范、路径与可移植性、项目书现行说明及经理最新交接。继续读取：
1. niuma/员工C/任务/第二轮/新增Turn-HTTP契约.md
2. niuma/员工C/任务/第二轮/联合验收矩阵.md
3. C 第一轮最终审查和 HTTP-SSE 契约、B 第一轮最终审查。
已读不重复，不从完整历史聊天开始。

本次用户给你这份有效任务即为派发来源。登记任务修订、接手时间和唯一执行标识到本人身份；核对任务板/当前差异，不因任务板尚未同步重复索取确认。有证据表明同任务旧执行者仍在修改时，协调重叠范围并继续独立阅读。

Git使用共享工作树过渡模式：工作树 ./，当前分支 main，HEAD 8f0ccdc79bf5443caf53e0bf98d455ffd9ef9da5 加有效未提交成果。核对 niuma/项目经理/审查记录/2026-09-23NEXT-01工作区基线.json 中的86文件清单。旧路径D和NoManCode/niuma未跟踪来自未提交迁移，不能reset/clean或从旧HEAD新建缺成果工作树。经理是唯一暂存、提交、切分支和整合执行人，你只读Git并交报告。若经理已发布修订2，以其中真实新基线和工作树为准，不自行转换模式。

唯一源码写入范围：NoManCode/rust-app/src/server.rs 和新增 NoManCode/rust-app/tests/turn_http.rs。可更新自己的第二轮提交报告、有限脱敏证据与个人身份；任务与review由经理维护。不修改旧http_contract/session_turns测试、Engine、Store、Repository、domain、secrets、protocol、schema、Cargo/锁文件、构建脚本或UI。遇到必要跨层依赖给出具体反例和建议，由经理修订；不越权修底层。

挂载 POST /api/sessions/{id}/turns，严格三字段body，Session只取path，Idempotency-Key必填且唯一。按契约处理重复头、逗号合并、ID和UTF-8长度、安全拒绝与body提取错误。格式合法后仅调用Engine::send_chat_turn，保留先回放后资格/路由检查的顺序，不在HTTP层另查latest、拼SQL、生成业务ID或实现幂等。

新建201、回放200；返回准确turn、TurnTask和replayed。专用错误转换穷尽ChatTurnError及IdempotencyConflict；只typed NotFound为404，未知错误安全500。禁止按中文判断，不能用默认ApiError转换吞成400，也不能直接套read_error。配置/路由普通anyhow本轮安全500是明确决定。

按联合矩阵逐项实现有效测试：真实HTTP+本机mock、稳定基准后的单故障、11表/旧行副作用、Provider调用差值、同key/异key竞争、准确Session SSE、运行中断线与真实partial重启。旧读测试seed_chat不是可追加的成功夹具。不能以sleep、提前失败或只断言is_err代替目标场景。

由你完成实现、调试及正式测试，跑任务规定的test/fmt/clippy/wasm-check，保留实际退出码与日志，付费live保持忽略。经理本轮只做契约与review，不代写实现。A/B/D没有因此并行开工。

交付 niuma/员工C/提交报告/第二轮/员工C（新增Turn-HTTP接入 第2.0轮报告）.md，附件放同目录NEXT-01证据/。逐项写验收编号、实现入口、测试名/观察、哈希、命令与结果；失败/未执行/历史结果分开。源码路径正文以仓库根为准，Markdown链接按文件位置，不写个人机器绝对路径，不记录凭据。更新本人身份并把报告路径交回经理；报告提交不等于Git提交、验收、合并或发布。
```

完整契约：[新增 Turn HTTP](新增Turn-HTTP契约.md)；验收：[联合矩阵](联合验收矩阵.md)。提示词不取代任务单的精确边界。
