# NEXT-21A A0 attempt2错误码采集修正与环境试验

2026-11-01，项目经理。原B继续同一树和第二十轮，无实现接管。

静态核 `0fe116ab..42303ebf021a7a21cfd76ef56ea0b2d1288f56f7`：只改sandbox一个文件，持有work_dir UTF-16缓冲，并在CreateProcessW返回0的相邻分支立即获取错误码。旧版本在取码前临时Vec析构，不能排除last-error改变；因此attempt1的203只能称失败分支观察值，不能单独证明CreateProcessW原始码。旧日志和原审查保留，本补记修正证据强度。

attempt2产品 `4f84ef4823bba0df5b9eac2a310afd409021404a`、Rust `8a940a0b81eb20446ced0741f550caede83611c9`，北京时间07:03至07:04 check/build exit0，S01仍exit101，立即采集得到203；依然没有created_suspended/PID。后三项及fmt/Clippy未执行。这次能将203对应到API失败返回，仍不能判定具体缺失变量或隔离能力。

经理核旧attempt的37个文件未变；新五清单185输入一致，与attempt1仅sandbox一个输入不同且对应新提交blob；新37文件已登记hash，详 [核验](../../../项目经理/审查记录/2026-11-01NEXT-21A-A0attempt2核验.json)。probe构建记录摘要为 `85c63305d44ade75df1e1123f360182d8dbe6963937bf98a46214614f8b67e8a`，不能将旧target随后变化改写为旧受测二进制。

允许原B下一最小环境假设试验：仅增加显式LOCALAPPDATA，值为本次staging/work；不指向真实用户profile、不继承完整环境、不改变目录只读ACL、LPAC/Job或断言。新提交、新attempt按既定顺序首失败停止；即使成功，只证明该差异与当前启动结果有关，不宣称OS内部根因已证实。此次属原授权的普通启动修复，不是扩大插件允许存储范围。

原C另获第二十三轮WASM执行/真实绑定只读预审，B仍唯一产品和动态执行人；D冻结。主线产品仍NEXT-20，A/外部运行时/完整核心均未完成。下一第一操作核单变量候选差异与实际结果，再决定A的下一修正或完整矩阵。
