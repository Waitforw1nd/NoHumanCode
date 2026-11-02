# B23 case16定点回收预检attempt1：未就绪，原对象持有

2026-11-02，员工B Jason。按[经理预检签定](../../审查记录/第二十三轮/2026-11-02B23-NT核收与定点回收预检签定.md)只运行固定包编译与预检；未创建01–05请求、未终止任何对象、不清gate、不重试。产品HEAD `98cec2e441adcb7cd0476c81db075c8f1ce4659e` / 产品 `85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e` / Rust `951cff6854e0a8bc0b7cc48d559b735c60256c21`，启动前后clean/index空。

编译器原PID `14452` / FILETIME `134381061959798864`，原handle signal `2026-11-02T15:16:36.6916732Z`、exit0；固定源SHA `1a5238c47e2cedd9b813e221bee40ddf6462abd5e6d8fd5d28153f243a2a5b00`，生成EXE 376832字节、SHA `e17ba79d0d32805b3282d61b91ccebda293de6c522bae81d911f02a37a0ebd10`。固定MSVC14.43.34808 Hostx86/x64与SDK10.0.22621.0，工具/DLL哈希保留environment.json。

回收工具原PID `44296` / FILETIME `134381061967575472`，exec session `47108` 仍运行并持原工具handle；stdout为：

```text
identity role=case pid=89780 creation=134381029069968888 wait=258 path_match=1
identity role=runner pid=62476 creation=134381028898031298 wait=258 path_match=1
identity role=supervisor pid=87376 creation=134381028875429933 wait=258 path_match=1
identity role=launcher pid=90776 creation=134381028866695431 wait=258 path_match=1
bind stage=runner_to_case seen=546 matching_original=1 errors=0 walk=259 marker_free=0 snapshot_free=0
bind stage=supervisor_to_runner seen=528 matching_original=1 errors=0 walk=259 marker_free=0 snapshot_free=0
bind stage=launcher_to_supervisor seen=530 matching_original=1 errors=0 walk=259 marker_free=0 snapshot_free=0
RECOVERY_UNKNOWN reason=initial_job_helper_proof_failed retained=9
```

四个身份/路径与三段父持有原handle比较通过；但首次 `!sourceStillOriginal() || !jobAndHelper()` 为true，程序没有输出细分子项或Job/helper字段，不能断定具体哪一次复制、类型、原对象比较、helper映像、signal/exit、成员、accounting或flags失败。没有`PREFLIGHT_READY`，不得进入任何终止阶段。`retained=9`为工具输出，不凭数值推断各handle角色或释放安全。先前NT只读观察的Job active0/total2/flags0x2800及helper wait0/exit0/in_job1仍是当时对象快照，不证明此次复合条件中哪个成立。

当前原matrix exec session `26580`仍运行；两gate仍存在并只读核原字节SHA：common `026f1fe719552fe2186bfa74a6660f9ee34e079671c91dd9e39e5990f1073109`，inner `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`。01–05阶段请求均不存在；原工具stdout 726字节SHA `53488935edd7f7cd07956f2928095b7128307a9bba887ef03b1b61d7ee59fbbf`。持有中非exe/obj/tmp原记录13份42099字节，源/副双hash发布[清单](B23-case16-RECOVERY-preflight1停窗快照/双hash清单.json) SHA `4ca2810bd563dc4d9853d2bbb123d2651aca3a599ae002547a09775b3516408a`；这是停窗快照而非终态。

本次未发生case/runner/supervisor/launcher终止；恢复工具与其9个副本仍持有，case16原unknown不变。下一步经理审核复合失败及持有对象，另行裁定是否需要可区分子项的方案和原工具副本处置；不自行终止、释放、清gate或重启。
