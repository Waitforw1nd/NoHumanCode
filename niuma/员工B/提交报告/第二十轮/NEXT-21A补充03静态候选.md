# NEXT-21A 补充03静态候选

2026-11-01，员工 B。基线 `83b2e3a7a262ad31f0ca23dfaef1c55af10a536c`；候选 `568834561c42c808559b967e574644f7de592309`，产品树 `6b074449d71735fb3fcff29ba4fe07df1ef16622`、Rust 树 `b8e9a4c5ec96cd4a070d67f74b9d4cd9cc9a8a31`。独立树 clean；仅受控probe、隔离模块、新integration和其support四路径修改。限定fmt及diff-check通过，未编译或运行，交经理静审。

真实profile诊断在本次 `CreateAppContainerProfile` 成功后，使用该实例SID调用Host `GetAppContainerFolderPath`；要求绝对且存在目录，Host对固定无秘密哨兵使用create_new，记录路径、SID和初始摘要。专用 `run_profile_probe` 拒绝调用方预设profile路径，仅注入Host所得路径至有上限的测试JSON；探针分别直接读写既有哨兵、create_new并写新哨兵，同时保留自身profile位置查询结果。Host记录运行后摘要和新建文件存在事实，显式清理并删除本次profile；异常路径的Drop仅尽力清理，不能声称异常回收已证明。此逻辑不扩大ACL、capability或普通生产接口权限；诊断API只针对本次新建profile。

Job控件复用同一普通helper，唯一变更活动进程上限1/2，分别查询实际限制。上限2须可信短命子进程成功退出，上限1须记录1816；两项均经helper的Job清空与父进程信号确认。受限侧也查询实际Job上限1，child码须与上限1控制一致；此是Job数量限制证据，不是LPAC单独禁止派生证明。原Host进程访问仍要求5。

分项顺序建议：check、显式probe build、普通AC鉴别、S01、`s02_actual_profile_path_direct_access`、`s02_s07_file_sentinels`、`s03_s07_process_and_job_controls`、S05、S08、`s04_s07_network_loopback`；前项全通过才fmt check/Clippy，首失败保全。网络初始化10107使S04失败且操作未执行，不等于拒绝；外网/DNS、真实注册表配对哨兵、强杀和其他S01–S08缺口保持未验收。profile真实定位及两组Job控制尚未动态证明。
