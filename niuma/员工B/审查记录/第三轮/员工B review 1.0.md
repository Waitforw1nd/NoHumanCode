# B-R3-01 review 1.0 — CHANGES_REQUIRED（中间候选）

2026-09-26，经理 GPT-6；固定中间候选 `dd57adfc9321454651bdcdabb3a081bc95091600`，基线 `e7f6f98d7fceb0ee4004d9412e1c41c1724fa900`。源码11处在有效契约及补充1范围内。B明确尚未完成全部W证据，当前check0、定向4/4/AP19 2/2/node0不构成完整验收。经理与[A第1.2独立审查](<../../../员工A/提交报告/第四轮/员工A（Workspace固定候选审查 第1.2轮报告）.md>)确认以下问题，原分支新增提交修复，不重写候选。

| 编号 | 固定候选缺陷 | 修复与验收 |
| --- | --- | --- |
| B-F1 | workspace HTTP将原anyhow传ApiError，500可泄SQL/路径；两路由缺persisted-ID校验 | 所有映射固定安全文案，保留内部cause但不传响应；先共享ID验证，非法400；真实HTTP故障/秘密/ID反例 |
| B-F2 | 文件副作用后outcome或parent finish失败直接?，无本次unknown receipt | 统一故障收口，尽力持久unknown/partial，无法持久时也返回稳定restore_id的安全unknown receipt；测试outcome提交和parent提交失败窗口、重启事实 |
| B-F3 | atomic_replace的write/sync/pre-replace失败遗留明文temp | create后RAII cleanup，replace成功才disarm，错误/展开路径清理；注入相应故障检查无临时正文残留 |
| B-F4 | failed准备参与聚合，先failed后finished仍failed；restore把failed当unknown | 确定未写失败不参与成功before/after聚合；unknown/prepared仍阻断；覆盖failed→finished及finished→failed |
| B-F5 | legacy file_backup与新changes混合时忽略legacy，可能恢复任务中间态 | 有不可信legacy备份的task恢复Unrestorable，或先拒该task新write；旧占位正文不能消费。升级/resume混合夹具零恢复副作用 |
| B-F6 | parent partial/unknown后剩余outcome仍claimed，changes显示restorable=true | 同事务确定所有未完成逐路径终态，持有任意restore operation使task全部change不可新恢复；current仍表达磁盘真实状态，receipt表达历史 |
| B-F7 | 仅比较复制的binding，没有从原tool_call重算path/content/binding | durable task中查原call/name/args，重算审批绑定并规范path，对record.path/path_key/after逐项验证；独立伪造path/content/binding/call反例零写 |

F1～F5已提前逐条交B；F6/F7本记录正式收束。A的HTTP500与ID两项归并在F1，故本表7编号对应A8类问题。

BP01自锁、BP02生产协调、BP03全量解密预检、BP04父链复查、BP07schema结构主体在本SHA已修；最终测试仍需证明而非仅看代码。Windows短名/大小写、hardlink/reparse、真实停止runtime/子进程不能省略。基线原六处B-R2审批保证继续回归；本轮范围已独立升级。

W01～W14按原契约及补充完成，尤其W06/07/08真实故障与停止窗口、W09协调/封存、W10受保护秘密和输出扫描、W11迁移旧行/伪装约束、W12旧混合历史、W13真实HTTP完整receipt。B已承认未完成的项目是进行中，不伪记通过或失败。全部标准check/test/fmt/clippy/wasm-check，禁发布build/付费live。

新候选交完整SHA、逐项修复映射与报告，经理/A差异复核后再决定；当前不验收、不合并。
