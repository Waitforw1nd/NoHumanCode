# NEXT-14预算账本提交报告0.2

2026-09-28，B-R13-01-20260928-E01，员工B/GPT-6-astra。承接0.1及经理补充03/04；本报告绑定候选07acb202151bb86fe683d6781fee29f36f6c70b5 / NoManCodece132a3ad49e6155694bc43a3e68de3c0867302b / Rustbb05b198375b0bfd6dba0264af174629d3842652。提交状态不等于验收或合并。

## 交付范围

在c29/4e9/0ec基础上完成：schema15三表及严格marker/DDL验证；coverage高水位及规范request_digest；配置CAS/幂等与合法Project隔离；network claim与预算预约同Immediate事务；completion专用结算/恢复/回放/冲突；Engine四同步Budget API与NetworkError预算映射；checked聚合溢出分离结构与聚合校验；三真实Completion停点及工具/新Turn/resume矩阵；旧schema fixture精确空表/marker适配。新增tests/budget.rs为B唯一所有权。

D真实classifier已由经理串行合入；B未修改D源。C可消费真实同步方法：budget_config/configure_budget/budget_status/budget_call均Result<T,BudgetError>；三停点budget-before-claim/budget-after-claim/budget-before-finish仅Completion；Engine::crash_probe::arm现pub(crate)。

## 验证候选与实际结果

| 候选/证据 | 实际范围与结果 |
|---|---|
| compile-01/02（c29） | MSVC准备失败后只读现有VS根，`cargo check --lib --locked` exit0；D classifier尚未接Decoder，2 dead_code warnings保留；不是最终clippy。 |
| budget-unit-01/02 | 首次测试编译101因错误使用Permit unwrap_err（原件保留）；修后0ec预算8/0/0。 |
| budget-schema-03 / budget-matrix-05 | 基础预算+schema、双Store竞争、coverage/CAS、回放冲突、missing/invalid/unknown、故障回滚、B04聚合、三kill、合法双Project隔离等分别覆盖；dd7合并范围14/0/0，f735限定矩阵17/0/0。 |
| budget-engine-01 | 真实route-check无Task、模型不扣预算；真实Agent工具轮/新Turn/resume/idempotent回放3/0。missing/invalid仍保留模型与合法read_file工具结果但阻下一completion。 |
| budget-kill-01/02/03 | 首次after断言漏显式recover，失败101原件保留；9cd修复后1/0；07ac等待完整scoped两行信号后1/0。经理复读确认crash_probe原staging→rename原子发布；07ac是冗余防御增强，非产品缺陷修复。 |
| old-fixtures-01/02 | dd7 approval_gate35/0；checkpoint首4失败因精确旧snapshot缺三空表；370后checkpoint15、project_context7、project_migration3、runtime10、workspace_changes21均0；plugin并发迁移首101因marker数量预期，fb0后限定1/0；schema-old-02 Engine历史18/0。全部旧失败和未执行保留。 |
| clippy-component-01/02 | 首次101为saturating与可折叠if，2dae修后限定`cargo clippy --lib --test budget --locked -- -D warnings` exit0。 |
| fmt-component-01 | Rust cwd `cargo fmt --all -- --check` exit0。 |

总计本轮实际固证：11个限定检查点/单元组；每项绑定不同候选，未把重复测试折算；compile/toolchain失败、首次测试/clippy与旧fixture失败如实保留。原/发布log+json共份双hash见[证据清单](证据0.2/manifest.json)。

## U项状态与未完成

已动态覆盖并有对应证据：U02主要配置/CAS/首配busy/R>limit/0额度/降限改R/跨Project；U03双Store同Project额度竞争及不同Project；U04 route-check/Agent轮/新Turn/resume；U05真实D classifier由D承担，B只消费；U06正常/零/实际超R/错误/取消/NotSent/重复冲突；U07三停点kill+wait及恢复；U08 missing/invalid消息+工具继续但阻下一次；U01非空14→15/marker/坏DDL/并发/字节保全及旧fixture精确适配仍需完整全量组合复核。U09由C，U10旧系统组件，U11最终五门禁仍未执行。

限制：真实provider mock覆盖为本地网络；未做付费live、release、push、跨树merge或最终五门禁；D保留其workspace fmt1历史事实，不等B组件fmt0；07ac额外signal完整性等待不是对产品crash_probe的修复。

## 文件、证据、下一步

生产/测试写权均在任务单内，无越权源。主仓仅本报告/证据和身份，人员index不操作；工作树当前clean/index空/no-build。下一第一操作交经理按当前完整候选执行D→B→C整合并实核三tree，再由C唯一串行五门禁；B不自称APPROVED。
