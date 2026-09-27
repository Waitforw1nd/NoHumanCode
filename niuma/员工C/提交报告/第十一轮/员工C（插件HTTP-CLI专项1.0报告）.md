# C-R11-01 插件HTTP/CLI专项1.0报告

2026-09-27，员工C / GPT-6-astra，执行ID C-R11-01-20260927-E01。候选提交732902c2b931c4ee9432c1fd2cc8cefa753996ee；NoManCode tree d68b3593dbce6fcda96740d0b7645d9bc14d4e04，Rust tree dd14715f018d0af8e8c2c0effff2c87b51177d9b。经理合B/D后的输入fd8d5b66，后续ae6e661、732902c仅C测试修订。源码status/index空，无在跑构建/测试，暂停供经理review及merge。

## 实际专项结果

| 输入 | 命令/结果 | 状态 |
|---|---|---|
| fd8d5b66d5242d599d88296a1a92cf4b5b7198db | cargo test --locked --test plugin_http --test plugin_cli -- --nocapture；exit101，CLI5/0、HTTP5/1 | F05项目注册准备返回合法Project.root_path，被共享测试helper误判为插件路径泄露；未到kill |
| ae6e661a87d3493c68db68f04754ac4006c22f42 | 同上；exit101，CLI5/0、HTTP5/1 | 已真Host kill+wait/restart与disabled状态保持；引擎安全拒绝未授权read使Task failed，测试原先错误期待继续模型并completed |
| 732902c2b931c4ee9432c1fd2cc8cefa753996ee | 同上；exit0，CLI5/0、HTTP6/0 | 总11/0/0，无helper入口；F05严格确认拒绝分支及独立enabled新Run正控 |
| 同732902c | cargo clippy --locked --bin peachsh --test plugin_http --test plugin_cli -- -D warnings；exit0 | 限定目标Clippy，不是最终标准全量Clippy |

源码定向stable rustfmt及git diff --check退出0。未执行最终check→test→fmt→clippy→wasm-check，也未运行live/release/push。B五份schema fixture尚在适配，不能提前宣布最终验收。

## 本方矩阵实证

- F02：真实HTTP list/status查询全数据库各表逻辑快照不变；默认desired true/0、active真实providers两项及受影响/compat来源。旧内存disable后desired仍true、实际关闭、不收敛，查询不协调。
- F05：真实CLI禁用、记录Host pid2936、child.kill().await后child.wait()非成功（exit1）；同库启动desired原样、准入关闭/effects0/providers空、全数据库快照相等，无Provider调用。随后恶意模型read_file请求Task明确failed且精确未授权错误，无tool消息、无第二Provider、definitions不含read_file；启用CLI成功后独立新Run提供read_file且真实读到read-positive-marker，共计Provider3次（拒绝1+正控2）。文件字节不变、CLI当前目录无产物。启动中没有短暂activate由D组件的provider/consumer hook零次证据证明，C不据快照作此推断。
- F06：新key CAS+1、同key同参旧receipt回放；当前revision前进后回放不回退且不重新激活被内存停用的runtime。异参冲突/过期CAS逐次全表快照不变，同enabled新key显式协调+1；不同key同revision并发恰一200、一409。溢出由B组件负责，本方HTTP另证明超SQLite范围revision400无变化。
- F07：数据库trigger导致事务拒绝，安全500无原SQLite信息；全表快照/runtime不变，去掉trigger后原key首次成功，证明失败未消费key。commit后apply前及生命周期故障由B/D组件负责。
- F09：所有plugin GET/PUT token要求、Origin/Host/cross-site、重复/未知字段、null/负数/浮点revision、非bool、缺/重复/合并key、content type及body size、未知plugin固定404。逐拒绝全表快照不变。CLI实际子进程验证GET/PUT路径/字段/key/token、安全错误、不读本地数据、不自动retry，非收敛200完整原对象stdout且exit1、stderr空。真实Host PUT成功并持久接受后relay丢响应；CLI提示unknown，无重发；手动原key/原参数直达Host返回原receipt与replayed=true，全表未新增。
- F08继承行为最终由固定全量回归与B新增组件验证；本专项不冒充覆盖全部恢复/unknown/checkpoint。F11未开始。

## 变更与证据

生产保持首候选9bc2a4f，只五个限定文件；main未改，旧http_contract仅schema断言引用SCHEMA_VERSION。后续两次修订只纠正F05测试准备/既有拒绝语义，不改变生产或降低禁用要求。

证据位于[732902c专项manifest](C-R11证据/732902c专项/manifest.json)：3attempt及Clippy各原始log+运行meta，共8对SHA256。meta记录实际UTC/命令/完整HEAD/Rust tree/前后status/env/exit；原件在独立树.local/logs保持，发布副本仅绝对路径相对化及机器MSVC源占位，清单列转换。失败两轮不删除，不拼成成功序列。所有target/TMP/TEMP/日志均活动E树，已有MSVC只读使用，不称其已迁移。

下一第一操作：经理review/合B最新候选后核新SHA/tree/status，再按明确专项或最终冻结指令执行。本人源码暂冻，最终五门禁仍须经理固定完整SHA后C唯一串行、失败即停、修改从check重起。