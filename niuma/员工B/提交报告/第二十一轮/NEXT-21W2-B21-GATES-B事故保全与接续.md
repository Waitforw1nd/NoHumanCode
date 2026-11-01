# B21-GATES-B attempt1 事故保全与接续

2026-11-02，员工B同角色接续。经理核验本会话 `gpt-6.1-terra/high` 并授权事故保全；原B已停止，未开其他agent。本报告只记录现场和下一窗口候选，W2六门禁尚未完成，未获新窗口授权。

## 事故与现场

attempt1 的 check 和 debug Host build 原进程记录 exit 0；workspace test `--test-threads=1` 记录 PID 55364、creation FILETIME 134380268564124770、state running，exit 和 end 均为空。最后 stdout 于 UTC 2026-11-01 17:18:25 停在 `w10_present_result_with_unrelated_offline_root_is_unavailable`；该行没有完成结果。原 supervisor PID 46820、creation FILETIME 134380268179861942；没有 window-result、source-final 或第4至6门记录。用户交接称原工具 session 32621 轮询返回 Unknown process id；此处没有原 session 的新终态回执。不能补称原句柄 signal、workspace test通过或全部后代已清。

本次 UTC 2026-11-01 23:01:35 的 [进程快照](B21-GATES-B-attempt1原始证据/进程快照.json) 排除自身查询 PID，按原PID、B路径/cache及 cargo/rustc/git/cmd/pwsh/powershell/peachsh/wasm/test 名称筛选：原两PID均不存在，未见B路径匹配；其余五个可识别 cmd PID 为 22292、45048、1088、76140、73464，父PID、可执行路径和创建时间见快照。无匹配 cargo/rustc/git/pwsh 或固定测试/worker可执行进程。无关 shell 参数未记录；快照仅是时点观察。OS 上次启动 local 2026-10-31 08:39:59，早于窗口，不能以重启推定清场。

锁仍在原位置，SHA-256 `a810bed31bfb6ad756df4f901cadcd976f7db078ee6bb59ca31c5c4d3e7c103a`；stdout `3329d6aac9087dec4729c21611166702188e2b236f13d5544609361123186c43`，stderr `991ff0f069cbabe8ce1c6e6fb30d7d1e897b6e70a62f03750ef8885fb45a0dcb`，均与交接一致。旧 GATES-A 原件与旧清单未更改。

## 发布与核验

[原始证据目录](B21-GATES-B-attempt1原始证据/)含 attempt1 顶层全部28份原件、prepared 三脚本与 manifest 四份及原锁副本一份，共33份、251515字节；不含 EXE、target、tmp。复制前后源与副本 SHA-256 及长度一致，第二次独立复核33组零差异。[双hash清单](B21-GATES-B-attempt1原始证据/双hash清单.json)来源按仓库根相对定位、副本按清单目录相对定位，最终 SHA-256 `0a39f861339cc88a91d79c53d05153e0bf5514f7b82f566816d3a7e9d874cfee`；[发布复核](B21-GATES-B-attempt1原始证据/发布复核.json) SHA-256 `da22f14718b8ebb3b52b0df6f5c259489c2b9ce5cfe0f6af4180e4282bdaff29`。原文件内容及其内部绝对路径逐字节保留。

B 独立树分支 `codex/b/wasm-worker-process`，HEAD `f898f0b372bada37e2b962890c3dca23935c2f40`，产品 tree `77d0ecdf81b4ad90029bbc85db06271cc4478276`，Rust tree `43a9b604fb5e21b111cc831d98d2a92794d0962f`，clean/index空；[源码复核](B21-GATES-B-attempt1原始证据/源码复核.json)比对窗口前192条跟踪文件的长度和 SHA-256，零差异，文件集合一致。三份 FULL-C fixed worker、probe、lib-test 与经理记录的 `05d1a4b1…`、`6691e01d…`、`6b98bbaa…` 均一致；attempt1 独立 Host 副本仍为 `4989c227468ca477da3d31a4aeca0116cd88c6d8455daa7506556c9859f5a29a`。完整哈希见[工件与现场复核](B21-GATES-B-attempt1原始证据/工件与现场复核.json)。FULL-C 的21项和其限定范围不变。

共享 target 是可变构建输出：当前 debug Host 为 `e34312754b08e514feae01520f5cb311e37c984d7486e93ab1cebfc3e732ec32`，mtime UTC 17:15:55；worker 为 `efeff9cf4b4e436c1258d6ac47f1b93ca1d375e082c49f856a1ed1daa3b471a4`，mtime 17:16:04；probe 为 `f3baec1600a1b5eb86cc0cb7294bae992c4eac4a9b66ee1cc3ba7c015f2ec869`，mtime 17:15:34；缓存 lib-test 仍一致。第三门 stderr 记录编译并完成 test profile 后运行 lib 单测，时间与缓存变化同处窗口，但没有逐文件生成回执，具体来源未完全证明，不将差异称为产品变化或人为修改。下一窗口须重建 debug Host 并复制独立副本、记录源/副本 hash。

工具链只读核验 rustc/cargo 1.98.1、MSVC 14.43.34808、SDK 10.0.22621.0，已安装 wasm32-unknown-unknown；未下载或升级。prepared 四份哈希与经理复审完全一致，manifest 为 `9cc4dba82ac2a63d18e4ab44e79949e4a2f079cb10ec3b8f72dbafe9817cad31`。

## attempt2 候选，未签窗、未执行

以下从 B 树 `NoManCode/rust-app` 的 PowerShell 会话执行，调用者须持续持有工具会话；仅供经理复核和后续签窗，不构成启动指令：

```powershell
$attempt = Join-Path (Get-Location) '.local/b21-w2-gates-b-f898-attempt2'
$cache = (Resolve-Path '../../../nhc-b-tool-call-application/NoManCode/rust-app/.local/next19-b-a7fe2626-attempt4/target').Path
& (Join-Path $PSHOME 'pwsh.exe') -NoProfile -File '.local/b21-w2-gates-b-f898-prepared/launch.ps1' -AttemptDir $attempt -TargetDir $cache -ExpectedHead 'f898f0b372bada37e2b962890c3dca23935c2f40'
```

当前 attempt2 尚不存在；launcher 自建全新 attempt2 及其 `tmp`，在 Add-Type 前设置 TMP/TEMP，supervisor 继承并再次设置。停住的 W10 测试 `Fixture::new()` 每次调用 `tempfile::tempdir()`，source/worktrees/host-data 均置于该新临时目录；静态检索未发现 Rust 源或测试引用本事故 attempt1 或缓存路径。该检查不替代全部测试的动态现场证明；不复用 attempt1 fixture，缓存只作为构建 target。无新 runner 或产品修改。签窗前经理裁定当前锁、审查缓存/进程观察并另行授权；首个非零或未知按原已审 runner 保留现场停止后续，不重跑21矩阵。
