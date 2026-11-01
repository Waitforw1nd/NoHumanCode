# NEXT-21 W06-P01 额外 Job 消息取证预审（C-R32）

2026-11-01；员工 C。只读固定 `dbba89e90736696f1a9bc0bf4520495f260b1519`（产品 `4c9b80eff52d0f338297717b51d8d767d9cf81ad`，Rust `bee7ae8505d9878106228d00edb2cf1bd6282090`），main 基线 `5cc0bddb352d907f4954d46c6ca49ba85eb63583`。原件为 B 独立树 `NoManCode/rust-app/.local/next21w2-b-dbba89e9-attempt7/logs/09-resource-stage.log`。未运行进程、探针、测试或构建，未修改产品。

## 先行结论

历史 12952 / 21912 无对象句柄及创建时间等证据，身份、具体触发 Job、与目标的父子关系未知，不得事后按 PID 补判。新现场的消息同样是调查线索；但可在目标被有界握手保持存活时，独立通过同一 Job 的实时列表、进程对象句柄、活性和成员查询取得有时点的当前事实，不必先证明当前对象就是历史消息对象。W06-P01 仍开放。

固定代码 `src/wasm_worker_process.rs:1395-1455,1803-1808,1860`：test 端口在新建 Job 之后、进程创建之前关联；completion key 为数值 1，JobEvents 在 Drop 时逐条取队列、等待 100ms 即停，记录 message、lpOverlapped 数值而非消息产生时间/对象句柄；Job 被传给创建属性 Job list。完成端口本身不是进程对象，key 标识关联接收方，不标识究竟哪一嵌套 Job 触发。Drop 中单次 timeout 或失败均停止且无错误区分；这不足以证明没有遗漏或队列全程受监控，也不能把日志时间当事件时间。

本机 windows-sys 常量核得 6=NEW、7=EXIT、2=END_OF_PROCESS_TIME、9=PROCESS_MEMORY_LIMIT、4=ACTIVE_PROCESS_ZERO。原件 CPU 目标 60976 的 6/2 与额外 12952 的 6/7 同队列；内存目标 62072 的 6/9/7 与额外 21912 的 6/7 同队列。4 的参数 NULL 被旧代码打印为 `pid=Some(0)`，这是消息专属 NULL，不是 PID 0 的进程证据。原件中 Job active0 发生在 Drop 收割消息之前；它证明该次查询时计数结果，不证明此前域内从未有其他对象。

## 最小实时采集建议

1. 沿真实 W2 创建流程，在新 Job 尚未激活时关联端口，启动有 ready 握手的消费者；每个 Job 使用明确的本次关联标识，记录关联/取队返回、完整 message、key、原始参数、时间及立即保存的错误。按消息类型解释参数；区分无包超时、端口错误和异常 key。保持生产 active1、资源限额和创建标志不变。
2. 真实目标通过既有创建返回的进程句柄确定身份，读 PID、GetProcessTimes 创建时间、映像并保持查询/同步句柄；在允许的测试探针代码中让目标于消耗资源前有界等待，测试侧准备好后放行。不得令探针无限等待或延长生产截止；失败时走既有收割、报告 unknown。实时消费者不得只在 scope Drop 时取消息。
3. 目标存活区间内查询同一个实际 Job 的 `JobObjectBasicProcessIdList` 和 accounting（total/active/terminated），记录调用起止、返回、所需长度、assigned/listed 数及完整 PID；缓冲不足按返回信息有界重试，超限/失败记 unknown。对各列表项尽快打开最小查询+同步句柄，记录请求与结果及错误；在持句柄期间 GetProcessTimes、QueryFullProcessImageName、等待状态、IsProcessInJob(该真实 Job) 分别核实，失败保留 unknown，保持句柄直到取证结束。再次查询列表/计数，并再次核目标及已获取对象存活和成员；记录各步骤的时间区间、完整结果和句柄对应的创建时间。禁止根据映像路径或父 PID 独立认定安全/可信，若需要文件真实性必须另有独立证据。
4. 如果额外对象在持有其句柄时活着且 `IsProcessInJob` 对同一 Job 成功返回 true，可以认定该对象在查询时属于该 Job；若其与预先持有的目标句柄是不同对象，且目标在覆盖该查询时刻的前后两次等待均未 signaled，可证明在该时刻存在两个不同存活对象及已核成员关系。列表重复出现和前后计数为交叉佐证；各 API 非原子快照，单靠两次列表相同不能证明中间无短命对象，也不能用结束 active0 证明整个区间无额外对象。嵌套时 membership true 包含层级关系，不能单独确定最深所属 Job 或消息触发层。
5. 对首次看到的通知 PID，OpenProcess 后得到的对象只能描述打开时取得的对象；如果源事件时没有独立锚定它存活，不能排除退出和 PID 复用，不能将后开句柄无条件与消息绑定。目标自己可用 CreateProcess 返回的句柄作为锚；未知额外对象若过快消失或没有独立对象锚点，消息归因继续 unknown。短生命周期可能被轮询/非保证通知漏掉；未观察到不是无后代证明。
6. 若需要在下一次现场验证控制台辅助进程的可能性，只将其作为待查假设：不预先假定其为 conhost、无害或限额豁免；以实际取得的对象、映像、创建时间、Job 成员/计数、启动环境和测试内受控对照说明。当前未找到可核验的相关微软 Q&A 原文，不能把该假设写成官方保证。不得改生产 flags、放宽 active1 或任意运行子进程来使现象消失；若无法获得证据，保持阻断。

采集线程和握手的取消、超时、失败路径应有限且能汇报原始错误；只持进程查询/同步句柄的外部观察者不持 Job，避免影响 W05 最后 Job 句柄的条件。具体实现与动态由 B 独占，经理核验；以上不扩大独立 launcher 或生产行为。

## 依据与证明边界

- [微软 JOBOBJECT_ASSOCIATE_COMPLETION_PORT](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_associate_completion_port)：嵌套触发 Job 的通知向其父 Job 链关联端口传播；lpOverlapped 随消息类型代表 PID 或 NULL；普通消息投递不保证，关联时状态变化也有竞态。
- [微软 JOBOBJECT_BASIC_PROCESS_ID_LIST](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_process_id_list)：列表涉及该 Job 与其子 Job，返回 assigned 与实际列表数量；容量不足需扩大缓冲。列表和独立对象查询不是原子事务。
- [微软 Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)：Job 查询、限额、通知和计账的总体语义；通知缺失不证明事件未发生。

更新：本报告未证明 attempt7 额外对象的身份、关系或来源；未证明没有其它短命对象或完整 W06 已通过。下一步等待经理转交 B，C 释放。
