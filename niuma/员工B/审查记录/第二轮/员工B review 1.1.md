# 员工B review 1.1：审批修复及组合门禁

日期：2026-09-25；项目经理GPT-6；员工实现与正式测试gpt-5.6-sol。受审B修复 `7611683b742066f8ac9f553f62c1e40161014c0d`，C支持 `891263a4e9628affbf071a7d2ceb8ee71ef137c8`，经理串行组合版本 **`0b44eda5ded2aeb2d5d5930df5775070dd270105`**。

结论：**APPROVED（B-R2-01 / NEXT-02C 修订2、含明确授权的AP12旧测试适配范围）**。原review 1.0的三个问题在此组合版本闭合。不是整个产品完成，不含审批HTTP、插件实际沙箱、Workspace/Diff或CLI；实际整合另记SHA，未推送/发布。

## 问题闭合

| 编号 | 核验与闭合依据 |
| --- | --- |
| B-R2-REGRESSION-01 / AP12 | B保留迁移回滚数据断言，仅更新最终schema预期；runtime使用有界公开审批步骤，原路径/硬链接/命令权限/依赖环断言保留，路径错误改为更明确的JSON error字段检查；C仅6→7一行。组合标准test206通过、0失败、1付费忽略，原五项失败全部通过 |
| B-R2-CANCEL-01 | Engine持久取消成功后active可选；未知task仍报错。新增真正shutdown/reopen的公开Engine取消，pending与approved两分支、重复取消一次resolved/零execute，interrupted不非法转移。经理完整diff复核+A固定SHA独立静态复核+组合实跑通过 |
| B-R2-REDACT-01 | Store先结构化redact，普通稳定结果保留compact JSON；必要时仅对safe Value做Unicode编码，验证二次脱敏稳定和JSON解析同义。事件/持久消息/Engine内存使用一致安全值；原finished校验不放宽。真实命令退出7、bearer stdout、stderr、一次副作用及shutdown/reopen/resume通过；中文emoji/普通结果/错误对象单元通过，原AP16事务故障仍通过 |

A第1.0提供固定旧SHA最小反例，第1.1对新SHA复核两项闭合，见[A复核](<../../../员工A/提交报告/第二轮/员工A（审批候选安全复核 第1.1轮报告）.md>)。A第1.1未重复运行B测试；经理复核代码、日志、退出码及范围，不将自身第1.0员工实现经历或A静态审查称为独立全仓实跑。

## 固定组合验证

正式门禁由员工B在组合SHA下分别执行，经理读取原始日志和JSON、汇总各测试目标计数并核对SHA。证据目录：[第1.2轮组合证据](../../../员工B/提交报告/第二轮/B-R2-01第1.2轮组合证据/test.log)。实际时间2026-09-25T14:12:06至14:12:35+08:00。

| 标准动作 | 退出码 | 实际结果 |
| --- | --- | --- |
| check | 0 | 通过 |
| test | 0 | 206通过，0失败，1付费live忽略；含approval_gate31、lib62、runtime10、http_contract11 |
| fmt | 0 | 通过 |
| clippy | 0 | 通过；既有依赖future-incompat提示未冒充失败 |
| wasm-check | 0 | 通过 |

候选基线40da490至组合仅八处：原六处+经理有限授权的runtime/http_contract旧测试适配；lib仅模块导出。经理实际检查组合diff-check、工作树干净、共享主树无源码在途差异，main在40da490之后仅任务文档变化，整合前产品输入一致性可核对。不以旧29项自测替代当前31项。发布build/付费live/OS断电/生产用户库迁移未运行，符合本轮边界。

AP01～AP20及AG01～AG06的细项实现/夹具以B第1.0矩阵为基础，加本轮两项反例回归及组合日志；AP12在本组合正式闭合。AP15使用真实停止runtime，claim后执行前窗口由SQLite故障注入触发；不是OS强杀/断电，该限制契约允许且报告已披露。

## API冻结与限制

对后续C传输适配冻结本组合公开API。查询、决定、ApprovalRecord/ApprovalError/两个状态枚举沿B第1.0报告真实定义；唯一变更如下（Result为anyhow::Result）：

```rust
pub fn finish_approval(&self, task: &Task, approval_id: &str, result: &str,
    name: &str, tool_call_id: &str) -> Result<String>;
```

返回最终安全JSON content，供Engine内存复用。该内部执行协调接口不能作为HTTP直接执行入口。HTTP使用Engine::decide_approval及Store安全查询，未知存储错误安全500，不把所有错误映射404，不返回Debug内容；具体HTTP实现仍需新的正式派发。

保守最多一次自动尝试、unknown禁止重试、秘密参数无法恢复则拒绝、审批等待占并发许可、外部文件TOCTOU、取消不撤销已claim副作用、resume暂存输入再次崩溃需重新提供、未发布不兼容schema7拒绝均保留。安全JSON必要时编码导致长度增加/可读性降低，但解析语义保持。主数据库字节扫描不是WAL/自由页完整取证，不夸大启发式脱敏覆盖能力。

下一步：经理按Git规范串行整合并登记实际主树SHA及源码树一致性；保留原候选与报告。审批HTTP新切片此后可据冻结API准备最终任务包，不因本review自动开始实现。
