# B23 98ce BUILD attempt1结果

2026-11-02，员工B Jason，按[经理唯一BUILD签窗](../../审查记录/第二十三轮/2026-11-02B23-98ce预检核收与BUILD唯一窗口.md)执行。固定HEAD `98cec2e441adcb7cd0476c81db075c8f1ce4659e`、产品 `85020d9c3c34e9f498fc9f4e4b5f855b087c5f4e`、Rust `951cff6854e0a8bc0b7cc48d559b735c60256c21`；现场在员工树Rust `.local/b23-build-98cec2e4-attempt1/`。

exec session `36098` exit0；原supervisor PID `91040`、creation FILETIME `134381025056164399`，原handle signal `2026-11-02T14:17:11.0005366Z`、exit0。

| 步骤 | 原PID / creation FILETIME | 起止UTC | signal后exit |
| --- | --- | --- | --- |
| offline/locked probe example build | 80860 / 134381025353242112 | 14:15:35.3161270–14:16:38.7412128 | 0 |
| offline/locked lib test `--no-run --message-format=json` | 91648 / 134381026006106441 | 14:16:40.6083308–14:17:08.8277191 | 0 |

独立解析Cargo JSON唯一lib test executable为attempt target `debug/deps/peachsh-772c8a6873d0d381.exe`；probe来源 `target/debug/examples/process_lifetime_probe.exe`。两者源和fixed副本再核字节与SHA均一致：

| 工件 | 字节 | 源与fixed副本SHA256 |
| --- | ---: | --- |
| `fixed/probe.exe` | 335872 | `bcc63ab6b6dfc982033c2e052d9b88d900fb6fb07c982435206290c9897b67c3` |
| `fixed/lib-test.exe` | 40629760 | `95a58781a58981fda0fbc4e4bd9517548642800312c7c02ec448fbeccd0d2991` |

完整 `build-receipt.json` SHA256 `974a1dec4a2087a84c1dfb0cf741f9da2fd8755b581425469a92a42a89940582`；`02-lib-no-run.stdout` Cargo JSON SHA `0b2386a2e801d41d1fea81dbe21053a8f24d748b4b87ee4e7eea9b92119812ac`。before/两步前后/final六份源码快照各196项，文件SHA均 `8662e8fc9dae1b58c8ea64e6ed4ebe9a610141c277c787b882cf78579ac5d5ce`；工具前后清单均 `37beb921b8018f56424835f4c7876f83431e19672f4edc3a4287129ca642f245`。最终三对象不变、clean/index空，gate成功路径释放。实际环境记录jobs2、debug0、incremental0、offline/locked、MSVC14.43.34808 Hostx86/x64、SDK10.0.22621.0，TMP/target/CARGO_HOME在E盘。target末次工件清单SHA `b8186853b16729a11a95ca639f756f984122759de018e68fd2e6b5aa656eedfd`。

现场顶层31件及prepared四件共35份，源/副双hash与长度复核零差异，合计1077169字节；[双hash清单](B23-98ce-BUILD-attempt1原始证据/双hash清单.json) SHA `cfef586de98962e2cb3b6a4d0e89853dc3d9dadb17e9aacfa1f218118f77bcd7`。发布不含target、缓存、tmp或fixed二进制，二进制原件保留现场供经理核。构建与no-run未执行probe、libtest、Host或矩阵；不证明进程行为或完整U。下一步经理核原记录、工件、回执与矩阵外围，再单独决定矩阵窗口，不自动启动。
