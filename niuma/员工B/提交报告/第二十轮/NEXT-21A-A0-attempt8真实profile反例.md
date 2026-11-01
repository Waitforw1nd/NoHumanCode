# NEXT-21A A0 attempt8：真实 profile 访问反例

2026-11-01，员工 B。固定候选 `568834561c42c808559b967e574644f7de592309`，产品树 `6b074449d71735fb3fcff29ba4fe07df1ef16622`、Rust 树 `b8e9a4c5ec96cd4a070d67f74b9d4cd9cc9a8a31`。check、显式 probe build、普通 AppContainer 鉴别负控及 S01 均 exit0；受测 probe SHA-256 `78545d2c37f62c571243e244df63bc3acc2140e971e6392593ea6f29af99cbb9`。

真实 profile 分项 exit101，是实际隔离边界反例：Host 针对本次新建 profile 的容器 SID 用 `GetAppContainerFolderPath` 确认其实际目录，预置无秘密哨兵并将该路径传给受限探针。PID 58260 的实际token按本候选访问语义鉴别为LPAC、零capability、低完整性且在活动上限1的Job中。该进程报告对Host哨兵直接读取成功、写入成功，并成功新建且写入另一测试哨兵。Host独立观察到原哨兵SHA-256从 `1d34a3683cf4b6fc65d41f53d8034adc4683c5dafaf7e7056e7c7622ac4cf523` 变为 `b4fd9eea19b2ac47ebd79ab5d30d941860ad56b51efd424f522a703c17718f32`，且新建文件存在。此为本机本候选的实际直接访问，不是API查询错误、模拟ACL或可认为通过的拒绝。

受限进程内部的profile文件夹和注册表位置查询仍返回HRESULT `0x80070005`；该查询失败与直接访问成功同时成立。探针正常退出0，Host随后清理本次测试哨兵，报告 profile 删除 HRESULT 0；这不是Host强杀或异常路径的独立回收证明。普通AC敏感性负控与S01仍只证明其各自路径，不能覆盖此反例。

按首失败停止：文件、进程/Job、S05、S08、网络、fmt check、Clippy在本attempt均未执行；完整S01–S08未通过。运行前后185个源码文件清单一致、固定三SHA不变、独立树clean、结束无重型进程。原始日志、runner、结构化事件及stdout保留在独立树 `NoManCode/rust-app/.local/next21a-b-56883456-attempt8/`；`profile-events.json` SHA-256 `EA11222A698A63CA17EFF8811362A447D7A0D37B7B98C92F651012B977DA7F6F`，`profile-result.txt` SHA-256 `701D7CDF4D5AD1AD15409B8446C521880DA305E794DF0BA3A55E71373F32C0D1`，`profile-stdout.bin` SHA-256 `201E7D2813AC92FF24410E8ACDC1D15A80C8C12FE1AD90FC1E9107EC110C69B3`。

当前未更改ACL、隔离方式或断言；立即停止本轮动态，交经理审证并裁决满足原边界的后续方案。不能开放 process 或声称隔离可用。
