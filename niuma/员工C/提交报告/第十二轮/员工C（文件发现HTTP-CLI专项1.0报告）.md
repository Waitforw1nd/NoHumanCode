# C-R12-01 文件发现HTTP/CLI专项1.0报告

2026-09-27，员工C/GPT-6-astra；执行ID C-R12-01-20260927-E01。固定候选5d6b4d0dd0114e27140e4c30a3742df560394bc2，NoManCode51abdac9633018cf6a11d41c8aa9e3a419c6ea13、Rust42685703aa6998b58ea5fed429e23b8c59032c62。经理先合真实D/B至8de9081；本人N11-C01测试修订后固定验证。测试前后及归档时status/index空，无后台构建/测试，源码freeze供经理review/下一依赖整合。

## 实际命令与结果

- `cargo test --locked --test plugin_http --test plugin_cli -- --nocapture` exit0；CLI7/0、HTTP7/0，总14通过/0失败/0忽略，无helper入口。首次执行成功，无本轮失败attempt；首候选9db6405未执行仍是历史事实。
- `cargo clippy --locked --bin peachsh --test plugin_http --test plugin_cli -- -D warnings` exit0；这是限定目标，不冒充最终标准Clippy。
- 仅本人三文件stable rustfmt、git diff --check退出0。最终完整五门禁、live/release/push未执行。

真实UTC、命令、exit、Rust/cargo、前后status与SHA、环境见[专项manifest](C-R12证据/5d6b4d0专项/manifest.json)所列运行meta；两命令各log+meta共4对原始/发布SHA256，原日志保持于本人树.local/logs，发布仅路径相对化/只读MSVC来源占位。

## L06/L07实证

L06真实Host helper分别对read_file（保留旧正控）、list_files、search_files执行。每例均CLI持久disable后child.kill().await + child.wait()非成功，日志三条实际PID/exit1；重启后desired原样、admission关闭、effect0/providers空、全数据库逻辑快照一致，Provider调用为0。禁用恶意模型调用失败为精确“模型请求了未授权的工具”，无tool消息、SQL确认零tool_start/result、只有首个Provider请求且无后续调用。CLI显式enable后独立新Run通过，definitions提供对应工具，Provider总3次。list精确返回a.txt非目录；search精确单匹配src/a.txt、line1、text=read-positive-marker；原read精确内容保留，文件字节不变。三种用例共享helper不把循环次数计为额外测试入口。

L07真实HTTP version1.1.0、四affected顺序/两compat准确；两实例、effect4、订阅0、四provider_id=builtin.files及各tool key@1.0均断言。状态查询/旧key回放无额外协调和全表变化；既有token/Origin/Host/严格JSON/key/size/安全错误、DB故障、实际丢响应原key手动回放全部保留通过。

CLI真实子进程验证1.0与1.1的list/status/set完整JSON及human精确影响/compat；未知1.2、两方向分别affected或compat混搭、任意工具名拒绝，GET安全internal、PUT提示unknown且不自动重发。非收敛200仍完整成功JSON stdout、exit1、stderr空，真实输入及安全输出旧矩阵保持。

N11-C01已关闭：旧首候选将false receipt的current替换为active true，虽尚未执行但不一致。5d6b4d0仅修成功/混搭测试的构造，保留result(false)的disabled/rev1/current/converged，只替换version/affected/compat；混搭负例不靠其他字段无效触发。生产CLI没有因该修订改变。

## 范围与后续

唯一源码cli.rs/plugin_http.rs/plugin_cli.rs；server/main/schema/其余产品未改。E新树.local target/TMP/TEMP/log及工程target已准备；现有MSVC只读，debug0/incremental0/jobs2。独立源码C提交；共享index/依赖merge经理，未跨写或造stub。

L01组件真实来源及L02/L03/L04/L05仍由B/D各自组件和经理review闭合；本报告不声称整个NEXT-11或完整阶段一完成。下一第一操作：经理整合B/D最终固定候选后核SHA/tree/status，再按最终冻结通知唯一串行L08；源码修改后不能沿用旧最终结果。当前为专项提交待审，源码冻结，无执行进程。