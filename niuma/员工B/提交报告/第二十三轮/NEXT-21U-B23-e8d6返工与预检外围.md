# B23 e8d6 最小返工与预检外围

2026-11-02，员工B Jason。原[27c7 attempt1失败报告](NEXT-21U-B23-27c7-PRECHECK-attempt1失败.md)已记录原session/原句柄终态、E0618、43条非test lib dead_code、四份196项源码一致及未执行Clippy。经理已核并归档26原件，精确gate `8b49909f99848df64757b3b56087d566dea013b8fa687a2d26b05afc38b3a96b` 已由经理移入其attempt1原件目录；本人不碰原attempt、原prepared或gate。

独立树新正常提交 `e8d6a2bdd6adc932155fc9aba612dcff8f8be6a8`，产品tree `93abce5f31628d8464d836b1c56483b323a8509c`，Rust tree `5097c8c7ed84cef0aa1a1b6596d03812b5ca4573`；clean/index空。仅将测试局部`result`改名`settled`消除E0618，并对`lib.rs`的`process_lifetime`模块增加 `cfg_attr(not(test), allow(dead_code, reason = "Remove after the C production integration"))`，应在C生产接线后移除；保留test构建警告，未公开私有API或关闭整crate lint。lib模块顺序由rustfmt调整。新候选未Cargo/未编译/未测试，未知后续诊断仍可能存在。

新包为员工树Rust `.local/b23-precheck-e8d6a2bd-prepared/`，除HEAD/产品/Rust三对象替换外，逐文件静态比对与原三个脚本一致；原包保留。文件SHA256及字节：

| 文件 | 字节 | SHA256 |
| --- | ---: | --- |
| `GateProcess.cs` | 3909 | `367781c7113c3a27bb0e44c831a3efa32e4d45a249f1fdd3b30f20381663dd42` |
| `launch.ps1` | 9954 | `ce9ed41ac5c92726fc20fb78acc07a5e53b758045a34b6b631b843c235924edc` |
| `supervisor.ps1` | 12486 | `39ab442be72ecbf87c58f10116c981523da64d4886c6aaa28c14c5809ba9d1dd` |
| `manifest.json` | 723 | `6bf7b8c7b5b8210819dcecd55e2ed371e8705013572eb4537038a755136cf6fc` |

两脚本Parser零错误；两个改动Rust文件rustfmt check和提交diff check通过。预检仍只安排offline/locked all-targets check→Clippy，第一失败停止，指定MSVC/SDK、E盘缓存/TMP/target、原进程监督和共享gate的约束及局限同[原外围报告](NEXT-21U-B23预检外围静态候选.md)。新attempt/target尚未创建；未运行外围、未重启、无新签窗。下一步经理审核新提交与四文件、决定新attempt签窗；后续矩阵仍另签。
