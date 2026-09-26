# B-R3-01 review 1.4 — APPROVED（NEXT-03A限定范围）

2026-09-26，经理 GPT-6。按原契约、补充1～3及历次review审查固定组合 **`5c5fcb6ef15398c7f01967e5805a4fd2a144961c`**，Rust tree **`0d493bb20ba0c16e79dee2ee9693de394897bad9`**。本次结论为文件变更与安全恢复首片按范围验收，允许经理串行整合；本文件本身不代表已经合main或发布。

## 版本、人员与审查依据

B原执行 `B-R3-01-20260926-024830`、原基线 `e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`、`../nhc-b-workspace/` / `codex/b/workspace` 不变。GPT-5.6-sol初版、GPT-6-sol修复与中断历史保持；用户授权GPT-6-astra后，B继承在途内容完成定向补证并执行最终门禁。C原执行不变，GPT-6-sol提供崩溃/HTTP支持，GPT-6-astra只读审B固定差异。经理只核证、review、协调与Git整合，没有接管产品实现。

受审链条为B修复 `74229736ffca4f712a7564b3084110c51c58ea44` → `52364a9f2260ef9bc37a003284f7d05d1d7ebf77` → 最终补证 `2578029682e18eea911d5fae99717fb8beafe2ae`；最后在B明确干净冻结后合C HTTP `cf1a9c2c581985d1ce8a8907f04898df7c46c99f`，生成上述受测组合。该合并仅新增workspace_http.rs，无冲突；C crash文件与已验收CLI已在基底。

依据：[B第1.0完整实现与W矩阵](<../../提交报告/第三轮/员工B（文件变更与安全恢复 第1.0轮报告）.md>)、[第1.1组合门禁与覆盖边界](<../../提交报告/第三轮/员工B（文件变更与安全恢复 第1.1轮组合门禁报告）.md>)、[C第1.3独立只读补证复核](<../../../员工C/提交报告/第五轮/员工C（B Workspace固定差异只读复核 第1.3轮报告）.md>)、C支持限定[review1.0](<../../../员工C/审查记录/第五轮/员工C review 1.0.md>)/[review1.1](<../../../员工C/审查记录/第五轮/员工C review 1.1.md>)。经理已读取固定差异、实际接口、旧测试适配、日志及manifest；以下测试归B实际执行，不冒称经理或C本轮重跑。

## F1～F10与剩余补证关闭

| 项目 | 固定结果与关闭依据 |
| --- | --- |
| F1 安全HTTP | 两路由前置persisted ID校验、固定安全错误；真Host鉴权/Host/Origin/token/头、500私有trigger和HTTP/SSE敏感扫描均通过。 |
| F2 持久化失败 | FS后outcome/parent提交失败返回稳定restore_id的unknown receipt，尽力持久，不伪成功；重复调用不重做。 |
| F3 临时明文清理 | create_new后RAII所有权覆盖错误/展开清理，pre-replace失败真实验证目录无遗留tmp。接受该实现及动态反例；write/sync/OS replace错误未各自注入属于明确覆盖边界，不将其记为独立实跑。 |
| F4 失败聚合 | failed不参与成功聚合；真实第二读取故障的两种失败/成功顺序，恢复首次成功before；prepared/unknown仍拒绝。 |
| F5 旧历史 | legacy file_backup与新changes混合时整task拒恢复；不把脱敏占位正文当精确before。 |
| F6 恢复事实 | parent终态与recover同步未完成outcomes/change为unknown；complete/restored事实保持；存在任何operation均封存后续write。 |
| F7 绑定与脱敏 | 从durable原call重核工具、path、args、内容摘要和binding；独立篡改反例零写。脱敏丢失的新参数在prepare和FS前拒绝；秘密before仍DPAPI精确roundtrip。 |
| F8 真实约束 | SQL规范化保留引号内字面量；重建完整三表三索引夹具只改CHECK字面量，错误明确来自目标约束，排除缺表误因。 |
| F9 损坏摘要 | finished缺after或坏摘要typed Corrupt，不能过滤为complete/0；未知枚举解码拒绝。 |
| F10 prepare后读取失败 | 已知未写先CAS标failed，标记失败传播；测试核change/工具错误及recover后仍failed，零文件副作用。 |
| review1.3四组 / C-W09-E1 | 2578029补新建删除/外部改保护、超大及非UTF8输入、Host/scope变化、A→B→A再写、partial/unknown封存；真实生产future pin后poll Pending，锁内事实核对、释放后有界收集/取消，替换旧提前send信号。经理和C固定差异结论一致。 |

## W01～W14最终证据

| 契约 | 经理结论与精确边界 |
| --- | --- |
| W01 | 修改/新建经真实mock审批，typed changes与精确字节恢复/删除通过。 |
| W02 | 连续成功恢复首before；失败不污染聚合；拒绝/审批旧回归通过。 |
| W03 | 外部编辑、missing、非UTF8/不可读、非法链接区分；恢复冲突零写。独占锁不可读另由C执行期partial覆盖。 |
| W04 | A写/B写/A恢复冲突，A再获批写不吞并B字节；被外部改动的新文件不删除。 |
| W05 | 路径范围/禁止拼写/大小写规范化、真实hardlink哨兵保持；单项nocapture明确symlink创建成功且恢复拒绝。未宣称额外单独创建junction。Host变更及持久task scope篡改在claim前拒绝；普通save_task本就禁止scope变更。 |
| W06 | 第二路径预检冲突/密文损坏使第一路径零写；真HTTP恢复首路径后独占第二路径得到partial409、restored=1及持久逐路径事实。 |
| W07 | B前FS真子进程停止加C副作用后/finish前与提交后窗口；kill/wait/reopen，unknown不自动重试、finished不伪造。 |
| W08 | B恢复claim后前FS停止，加C恢复FS后/outcome前与complete后真HTTP子进程停止；稳定ID和逐路径恢复事实。 |
| W09 | 双恢复真实Pending屏障/单operation/同receipt；start/resume同gate等待，释放前无新run/原task变更，释放后收集并取消等待结束。complete重复幂等；partial/unknown Store构造仅证明后续写封存，真实partial/崩溃证据另列W06/W08。 |
| W10 | DPAPI秘密before精确恢复，事件/HTTP/SSE无明文、实际密文或内部绝对路径；解密失败/超大/非UTF8及不可重核新args零文件副作用。SQL原始状态排除公开changes过滤造成空集误证。 |
| W11 | 新库、7→8非空采样保全/重复open、AP19的6→7→8与迁移故障回滚通过。实际逐值比较tasks.value、approval id/status、event seq/kind/data；不声称每列字节快照。schema8坏索引列序及两种CHECK字面量有动态拒绝；其余完整DDL/索引属性/FK检查静态审查，未逐种另造坏结构实跑。 |
| W12 | 旧file_backup不安全恢复被拒、占位正文不写回；空无记录restore为complete/0且不创建operation。 |
| W13 | C真实HTTP三场景与B故障反例均在组合通过；400/403/404/409/500、空任务与未知task、安全DTO、partial/unknown receipt、稳定重复语义覆盖。 |
| W14 | 固定组合五标准门禁全0；AP31、AH14、runtime10、CLI11及其他回归通过，270通过/0失败/1付费忽略。未运行release build/live。 |

六个真实中断父场景来自B前FS两个和C后FS/提交后四个；C套件显示6 passed包含两个child入口，不能称C有六个独立崩溃场景。F3与W11覆盖限制已在报告披露，不扩张为全故障注入或每个坏结构独立验证。

## API、兼容与保证范围

Engine真实签名为 `changes(&self,id:&str)->anyhow::Result<Vec<WorkspaceChange>>`、`latest_restore(&self,id:&str)->anyhow::Result<Option<RestoreReceipt>>`、`async restore(&self,id:&str)->anyhow::Result<RestoreReceipt>`；业务WorkspaceChangeError可downcast，其他内部cause仍可能存在。公开DTO/error位于workspace_changes模块。GET顶层为changes/restore；POST成功顶层为ok/restored/status/receipt，稳定restore_id及outcomes位于receipt内。带receipt的409还含code/message/error/retryable=false。旧previous/current正文有意移除，web/app.js只作授权摘要/complete提示适配，node语法检查0，未做浏览器交互或视觉验证。

本片仅追踪批准write_file，恢复普通文件字节/存在性；<=256KiB UTF-8，新参数持久脱敏损失则拒绝。DPAPI失败无明文fallback；单Host共享Engine协调，不承诺多Engine/多进程互斥或抵御本机恶意写入者最后检查至OS调用间任意TOCTOU。任意非空恢复operation封存该task后续写，无自动reconcile/重试；不含run_command回滚、ACL/ADS/时间戳/文件identity、Git diff/目录checkpoint、Workspace CLI。

## 命令与证据核验

2026-09-26 +08:00，B在组合上串行执行：check 22:37:40.317～22:37:41.263；test 22:37:41.279～22:38:39.962；fmt 22:38:39.968～22:38:41.122；Clippy 22:38:41.125～22:38:43.147；wasm-check 22:38:43.150～22:39:31.445；退出码全部0。随后精确symlink测试1/1及node语法检查0，不改变源码。

经理核[Astra定向manifest](../../提交报告/第三轮/B-R3-astra证据/manifest.json)8份和[组合manifest](../../提交报告/第三轮/B-R3-组合证据/manifest.json)9份原始/发布SHA-256均匹配；解析标准test日志22个suite/doc汇总为270/0/1。原scope夹具101和环境/服务失败历史保留，依赖future-incompatibility提示未隐藏。组合前后Git干净，Rust tree一致；C支持两个文件SHA-256与已审版本相符。

结论：本任务范围内阻断项已关闭，按NEXT-03A限定范围APPROVED。经理下一步以固定组合串行整合main并核Rust tree完全一致，再更新交接。整合/提交不等于发布；旧工作树、在途继承历史档案和原始失败证据全部保留。
