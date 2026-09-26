# B-R6-01 第0.2次修复与证据审查

2026-09-27，经理GPT-6；实现员工B/GPT-6-astra，交叉只读D/GPT-6-astra。固定200964993fa72389c73ee1d88fc07575fb23a073为当前已提交核验对象；后续在途修改不是固定验收候选。本轮仍需返工/补证据，未APPROVED。

N07-B04安全credential_ref改为stored/route、stored/legacy、stored/account及env路径，不放宽通用secret检测。D公开Engine原失败4项在修复后原样通过，原失败保留；C原HTTP失败已转为仅一个管理撤销错误类型预期差异，经理明确管理路由仅查active无grant时Required、Task绑定旧grant时Revoked，需GET原grant revoked与零发送双事实。

N07-B-D02幂等错链：固定network_confirm检查原授权subject/config_revision/credential_revision/binding_digest/purposes与原请求，跨subject合法FK反例已有通过；源码复核闭合。D01原activeifnotexists字面量漏洞已修，但不据此整体放行schema校验。

新增N07-B-D03/P2：repository::verify_schema_10 normalize删除所有非引号空白。`snapshot TEXT NOT NULL`和合法`snapshot TEXTNOT NULL`均得到textnotnull；后者是TEXTNOT类型和NULL声明，丢失NOT NULL约束。D只读检查SQLite语法给出可构造候选，经理核固定源码；尚未运行本例，要求B真实验证伪DDL可插NULL/现有open行为，再保留token边界或结合table_xinfo修复，补Store::open拒绝反例。不能把分析当实测。

N07-B03新增gate内try_acquire/Notify注册/RAII先放槽后通知源码方向正确，当前在途确定性poll测试已有员工通过报告；还需N04真实listener撤销先赢零发送与准入先赢既有请求可完成、后续模型轮拒绝的证据，不把127.0.0.1:9和call0独立称全N04。

N07-B05回执字段矛盾此前允许success=true/result=http_failure等，已要求原B在Store出口修一致性及Corrupt真实反例；当前新测试已有员工通过报告，等待固定SHA/hash。N09必须真实schema9→10全列/v2/Checkpoint blob保全及6/7/8连续迁移，不能只引用旧generic迁移测试。

旧runtime回归适配获经理认可：先原grant/相同model验证旧same ID/role/model/usage22，再改变模型断言旧Task Stale且零发送，保留URL拒绝。不降低原功能断言，不将旧Task改绑新grant。workspace_crash仅将真实授权setup移到settings/key保存之后。限定Clippy的store collapsible_if仍由B唯一修复。

下一操作：B提交全部范围内固定修复与日志清单，经理核源码和N01–N09矩阵后串行组合给C最终五门禁。
