# 2026-11-16 流式凭据防泄漏：首轮候选

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

普通文本每次push及时输出，不等待无空白长单词结束。持有状态只有最多11字节未确定前缀、128字节ASCII字段尾部和常量大小的模式信息；秘密正文直接丢弃，不随长度增长。输入按Unicode字符消费，不截断UTF-8。

干净完成且codec.finish成功后才释放未确定普通前缀。传输/codec失败或future取消直接丢弃该前缀，Drop不调用emit、不写事件。用户草稿的语义是“已安全发布的输出前缀”，错误/取消可能少显示最多11字节待判定尾部；已发脱敏占位保持。不会补发原始尾部或重试上游。Task.output和持久delta来自同一过滤流；raw assistant message仍走原safe_task_value策略。恢复只回放已持久化的安全事件，不保存过滤器状态。

过滤限于既有可识别模式，不承诺发现任意未知秘密。敏感赋值的空引用值、Bearer后多余空白等允许保守遮盖/规范化；普通无敏感模式文本保持。历史已经落库的事件不由本候选清理。

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
