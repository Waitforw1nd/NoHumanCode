# C-R13-01 独立Shell专项1.0报告

2026-09-27，员工C/GPT-6-astra，执行ID C-R13-01-20260927-E01。真实依赖由经理合入f246b86fb84d18278d3ccd0776bfa52fd1e1ef11。最终本方候选 **ef14f4e1fc76fecad74acf0c5869489fe5bfb7d1**，NoManCode **33211302d2cb5879de8620deafea5d77f5f1f7f2**、Rust **677fc9d6f7c44220dfcd1cc0a7e7d53df432cec8**。源码前后clean，当前index空，无运行构建/测试，冻结供经理整合。

## 实际验证与失败

| 固定输入 | 命令 | exit/结果 |
|---|---|---|
| f246b86 | cargo test --locked --test plugin_http --test plugin_cli -- --nocapture | 0，CLI10/0、HTTP9/0 |
| f246b86 | cargo clippy --locked --bin peachsh --test plugin_http --test plugin_cli -- -D warnings | 101；C测试MutexGuard跨await诊断 |
| ef14f4e | 同专项test命令 | 0，CLI10/0、HTTP9/0，共19/0/0，无helper |
| ef14f4e | 同限定Clippy命令 | 0 |

ef14f4e仅将S09测试读取captured Provider消息的MutexGuard放入词法作用域，替代显式drop，全部断言保持；生产未改。原Clippy失败日志与输入保存，不以最终通过抹去。只运行本人文件rustfmt/diff检查0；最终标准五门禁、live/release/push未执行。

[专项manifest](C-R13证据/ef14f4e专项/manifest.json)包含四命令各log+meta共8对原始/发布SHA256。meta记录真实UTC、命令/exit、完整HEAD/Rust tree、前后status和环境；原件独立树.local/logs保持，发布仅工作树绝对前缀相对化与只读机器MSVC来源占位。归档后本人逐对核hash一致。

## S09/S10覆盖

S09真实Host shell=false后kill+wait：最终专项日志pid33164、wait exit1；重启desired原样、admission关闭/effect0/in_flight0，全库快照不变，启动Provider0。恶意command首请求精确未授权失败、无审批卡、零tool_start/result、无根marker、无续Provider。随后shell仍disabled时独立Run真实read/list/search与经明确批准write四项：内容、list单项、search路径/line/text、write结果/文件字节均精确验证。再显式enable独立新Run先Pending、无ack批准exit1且无marker，显式ack批准后真实根目录shell-marker.txt= shell-once，stdout含shell-output、exit_code0，审批Approved/Finished/Acknowledged且仅一条。根marker位于src文件write_scopes之外，如实证明shell不受该scope约束。files状态始终相同且active，Provider最终共5请求，CLI当前目录无产物。

S10 HTTP当前files1.2.0+shell1.0.0固定顺序、各真实provider/key、affected/compat/dependent/独立两实例及effect；global key跨ID冲突无漂移、files禁用不改变shell，shell禁用不改变files状态，shell token/重复字段拒绝与旧files严格安全矩阵均通过。CLI四映射与三合法目录；重复/缺项/顺序/跨代/未知ID或版本/混搭/错误dependent/跨plugin响应拒绝；请求ID、receipt/current关联保持。human说明scope/无OS sandbox/禁用不取消已准入；shell非收敛完整成功JSON exit1及丢响应手动原key回放保持。旧1.0/1.1兼容、原read/list/search真实kill+wait与files丢响应回放保留通过。

claim后spawn前kill恢复Unknown、持久最终claim、typedunknown/worker取消与deadline矩阵由B/D组件证明；本方不把S09正控冒称所有进程后代清理或Job Object沙箱。最终S11须完整组合固定验证，本报告只是专项提交待审。

## 边界与下一步

唯一源码cli.rs/plugin_http.rs/plugin_cli.rs，无server/main等越界修改；ef14修订仅测试。E新树.local TMP/TEMP/target/log、工程target兼容目录均准备，Rust/cargo1.98.1，现有MSVC只读，debug0/incremental0/jobs2。不操作shared index/跨树merge、不清理旧树和历史。报告与身份仅主仓本人第十三轮。

当前源码和证据冻结、无进程，下一第一操作核经理B/D最终组合SHA/tree/status；仅经理明确最终冻结后C唯一标准五门禁，失败即停/修改后check重起，不预跑或无变化重测。