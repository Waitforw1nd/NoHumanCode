# 2026-11-16 流式凭据防泄漏：首轮候选及交叉审查返工

执行者：本轮员工A。派发依据为用户最新深审后继续开发及主线程指定的独立 security 工作树任务；不接手旧 B/C 草稿。基线 `bd31edeb80ab6801af6987ee50e719d14f457935`，分支 `codex/audit-20261116/security`，开工工作树 clean。提交由A执行，主线程负责整合及主仓身份同步。

## 真实问题与反证

旧链路为 `provider::Decoder::flush → CompletionLease::execute 的 emit → Engine::run_task → Store::event → SSE`。Engine直接对每个delta建事件，Store只对当前片段脱敏。因此合成输入 `s` / `k-synthetic123` 各自不会改变，拼接后却形成完整可识别token。替换CompletionCodec也能直接调用emit，故仅修改builtin Decoder不能封住Host边界。

负控保留在 `stream_redaction_all_splits_match_complete_policy`：明确断言旧逐段策略仍得到合成token，新的全切分断言必须输出完整脱敏结果。全部数据为合成夹具，无实际凭据。

## 变更与范围

唯一产品写集：

- `NoManCode/rust-app/src/secrets.rs`：共享token前缀；新增有状态 StreamRedactor，覆盖token、Bearer、敏感字段赋值、引用值/转义及Authorization scheme。
- `NoManCode/rust-app/src/provider_runtime.rs`：统一CompletionLease输出边界包装codec的feed和finish；替换codec不能直接把原delta交给Engine。输出sink失败后保持失败，不允许codec吞错后宣称完成。
- `NoManCode/rust-app/src/provider.rs`：既有公开complete兼容入口使用同一过滤器；Decoder保留原始message构造和现有持久化策略。
- `NoManCode/rust-app/src/provider_runtime_tests.rs`：替换codec、真实Engine/Store/SSE链路及失败/取消回归。

NoManCode以外唯一新增文件为本报告。未修改engine/store/workspace/main、schema、审批、网络授权、锁或旧B/C树；未改主仓身份。

## 成功、失败、取消、恢复与兼容

普通文本每次push及时输出，不等待无空白长单词结束。过滤状态持有最多11字节未确定前缀、128字节压缩后的ASCII字段尾部和常量大小的模式信息（含短Authorization scheme探测）。Token可能同时构成后续赋值字段，因此字段尾部允许暂存敏感字符，绝不输出该尾部；不是零敏感缓冲，也不保证内存安全擦除。状态不随秘密或普通单词长度增长。输入按Unicode字符消费，不截断UTF-8。每次Completion另外累计已发布的安全正文，作为返回message.content投影；这不是待判定原始正文缓冲。

单次execute干净完成且codec.finish成功后才释放未确定普通前缀；多轮任务必须复用同一个raw边界状态，仅最终任务成功才finish。传输/codec失败或future取消直接丢弃该前缀，Drop不调用emit、不写事件。用户草稿的语义是“已安全发布的输出前缀”，错误/取消可能少显示最多11字节待判定尾部；已发脱敏占位保持。不会补发原始尾部或重试上游。Task.output和持久delta必须来自同一过滤流。返回assistant正文改为本轮实际发布的安全投影，不再把raw正文交给Store单独猜测跨轮凭据；tool_calls保留结构并继续既有持久化检查。恢复只回放已持久化的安全事件，不保存过滤器状态。

过滤限于既有可识别模式，不承诺发现任意未知秘密。敏感赋值的空引用值现在保持空值；Bearer后多余空白仍允许保守规范化。Token形状的字段名会被保守遮盖，例如sk-password=opaque123得到[redacted]=[redacted]。历史已经落库的事件不由本候选清理。

## 检查与回归

已执行：四个授权Rust文件的独立rustfmt（edition2024、skip_children=true）成功；git diff --check成功；静态复核实际替换codec准入、事件写入、SSE读取与Engine错误/取消状态路径。

未执行：任何Cargo、编译、产品运行、真实进程测试；遵守主线程统一串行门禁安排。下面测试是已编写、未运行，不能记作通过。

- `secrets::stream_redaction_tests::stream_redaction_all_splits_match_complete_policy`：所有已知token前缀、Bearer、赋值、quoted/escaped值、Authorization scheme；每个UTF-8边界两片及逐字符多片；含单片完整秘密与旧策略负控。
- `secrets::stream_redaction_tests::stream_redaction_long_plain_text_is_streamed_and_state_is_bounded`：长无空白普通文本持续输出，跨长字段敏感后缀、长秘密丢弃、Unicode与状态上限。
- `secrets::stream_redaction_tests::stream_redaction_only_clean_finish_releases_undecided_text`：完成释放普通尾部，异常/取消直接丢弃敏感尾部。
- `provider_runtime_tests::stream_redaction_replacement_codec_reaches_store_and_sse_safely`：替换codec多片及feed→finish跨片秘密，真实Engine终态/assistant message/Task.output/持久delta/SSE回放一致；这些断言可穿透旧Host实现。
- `provider_runtime_tests::stream_redaction_codec_errors_and_future_cancel_never_flush_raw_tail`：codec.feed失败、finish失败、真实等待网络body时取消future；持久事件无合成秘密，输出与delta一致。

主线程在固定候选及既有Windows构建环境下优先执行：

```powershell
cargo test --locked --lib stream_redaction -- --test-threads=1 --nocapture
cargo test --locked --lib provider_runtime_tests::p03_both_engine_paths_consume_replacement_codec -- --exact --test-threads=1
```

首个filter预计选择5个新增测试（3个纯状态机、2个Windows Engine集成）；不得把筛选零测试当通过。随后按联合变更执行所需check/Clippy/fmt及相关Provider现有回归。网络夹具仅回环mock，不启动OS子进程，不访问真实Provider。

## 当前结论

实现和静态检查已完成，待主线程编译、定向测试及独立审查。本报告不宣称候选已验收，不改判历史unknown，不将完整U/W3或其他生产插件接线视为完成。

## 交叉审查返工（同日增量）

首轮提交为 `7c657ce37e82744933d1dd2141a1fc34a542d4a3`；主线程回报原5项stream_redaction测试通过，随后D和父审给出了新的具体反例。此通过事实仅适用于首轮5项，不能作为返工后的通过证据。

返工首提交 `26951f04` 已交主线程整合，包含secrets.rs、provider_runtime.rs、provider_runtime_tests.rs。后续提交补充本报告和messages全量、无emit fallback、普通输出三投影断言。未编辑B的engine/store/repository/domain。

### 已落实的反例与修复

1. `sk-` / `password` / `=` / `opaque123`：旧Token模式丢失字段身份，值泄漏。新Token模式继续维护有界字段后缀，后续赋值进入敏感值状态；输出为`[redacted]=[redacted]`。回归覆盖敏感字段的全部ASCII两片切分和逐字符切分。
2. `api` + 128个下划线 / `key=` / `opaque123`：旧raw128后缀裁切删除api，丢失api_key归一化。现在先压缩连续`_-.`再保留128字节；覆盖128、1024及4096长度，长普通字段仍立即输出。
3. `password="` / `"`：旧算法预先输出占位，与完整空值消息不一致。新增QuotedStart，仅确认值非空时发占位；真实Engine/SSE夹具也加入空值。
4. 跨completion的`s` / `k-synthetic123`、`password=` / `opaque123`、`Bearer ` / `opaque123`：每次独立finish会丢失未确定前缀或赋值状态。尤其`sk-password` / `=opaque123`能穿透“先各自脱敏、再外层脱敏”的双层方案。新增`CompletionLease::execute_with_redactor(self, client, permit, route, emit, &mut StreamRedactor)`，在raw codec统一Host边界复用传入状态，不创建、不finish。原execute使用本地状态调用它，成功尾部同步追加返回content。B负责Engine循环外实例及最终成功flush。
5. 返回raw message的第二轮`opaque123`无法由逐条Store策略识别：Host同步累计已发布safe正文，并替换返回message.content；保留tool_calls。只返回content而完全没有调用emit的codec，将final content送入同一状态再emit。任何emit调用（即使只留下未确定前缀）都禁用该fallback，避免重复输出raw内容。失败或取消没有返回raw message兜底。

### 新回归与整合依赖

新增纯状态机反例测试`stream_redaction_review_counterexamples_keep_assignment_context`。原长输出测试不再断言token字段为空，而是断言其尾部至多128字节、长token正文不发出、finish不泄漏尾部。

跨轮夹具通过实际list_files工具组进入第二轮；有emit和仅返回content两个分支都覆盖token、赋值、Bearer、token形状敏感字段、空值、Unicode及最终普通s。断言整个持久Task（包含messages）不含synthetic/opaque123，成功场景assistant正文拼接与输出一致；事件与真实SSE回放也必须相同。第二轮feed/finish错误及等待网络body时取消均断言安全草稿和尾部抑制。

`stream_redaction_content_only_and_streamed_messages_use_safe_projection`额外覆盖route check的本地execute：最终s必须出现在返回content；同一codec再走Task路径，验证fallback兼容和最终message投影。

`stream_redaction_plain_token_fragment_preserves_output_events_and_sse`覆盖普通`foo` / `sk-plain`必须得到`foosk-plain`。旧Store逐delta再次脱敏会改变结果；依赖B提供可信安全delta内部入口。该测试要求Task.output、持久delta、SSE三投影保真；单条messages仍受domain::safe_task_value独立策略影响，允许保守遮盖普通token形状片段，不将其误报为凭据泄漏。

B还需在任务最终成功时，把共享state.finish尾部同时追加到output、最后assistant content及事件后再保存；任何错误/取消/轮数耗尽不finish。新跨轮测试依赖此接线，不应在只合入A的旧engine上宣称整体通过。

### 本次执行与剩余

已执行授权Rust文件的独立rustfmt及git diff --check，均成功。返工没有运行Cargo、产品或真实进程测试。所有运行结果由父线程在完整组合上串行提供。

最终专项仍使用`cargo test --locked --lib stream_redaction -- --test-threads=1 --nocapture`，当前预计选择10项（4项状态机、6项Provider/Engine集成）；不得接受零测试通过。兼容检查保留`cargo test --locked --lib provider_runtime_tests::p03_both_engine_paths_consume_replacement_codec -- --exact --test-threads=1`。父线程需在A最终增量和B可信delta/末轮尾部均整合后运行。

剩余：等待组合编译、上述定向测试及D复审；既有历史数据与未知模式秘密不在此修复范围。重启恢复不持久化未发布的redactor状态，用户可见草稿以已安全发布的事件为边界；跨新的run_task执行恢复未确定状态不在本轮跨completion保证中。

## 主线程实测回报（2026-11-16，产品冻结后）

主线程已审查并整合A后续提交 `f81cec7aa536916f23ed4fb7a69f0fd51eb70c0f`，确认含整个Task防泄漏断言、安全正文重组、Unicode、无emit fallback与普通`foo` / `sk-plain`三投影负控；独立message的保守过滤限制保持上述说明。

主线程随后明确回报：最终10项安全回归实测全部通过，check退出码0。回报的受测标识为：

- HEAD：`89e3bb1d`
- product：`84e03b3a44024ef2114241a8613b4955746d8fa3`
- Rust：`13fda626af066d1932ca1fcc56a72585be7bfc8c`

以上是主线程在整合组合上的实测回报，不是A自行运行Cargo所得；A没有复跑或查看原始测试日志。安全专项采用前述stream_redaction筛选，实际数量为10项，覆盖本轮新增反例。本节更新前述“待组合编译/安全专项”状态为已由主线程报告通过，不把早先单候选通过结果混用到新的受测标识。

主线程正开始固定6项门禁，尚未提供其完成结果，本报告不宣称6项门禁或最终总验收已通过。产品修改保持冻结；A本次仅补报告并普通docs提交，等待主线程通知失败才返工。身份同步及最终复核仍由主线程负责。
