# NEXT-13 第0.5轮N13-D08观测修订

员工D GPT-6-astra，2026-09-28；D-R11-01-20260927-E01。独立树 ../nhc-d-git-worktree / codex/d/git-worktree，本人仅提交git_worktree.rs，经理唯一跨树整合。经理将失败C935候选普通合D后，实核接收2a4596649dfaecd8e830c63602d5a4240e1f2286 / NoManCode d2ff69a800beb21487a40e8a268544e24c1a549c / Rust c48c78b987fb6af018e25f2cf8e37e4ee4f97340，clean；两源码树与935精确相同，随后按授权解除冻结。

最终D候选 **527f4661f544a1d1da2c4d9939a2f182268b5eba** / NoManCode **57f073b89e0629c4c0b36506db805e36afdfdd3b** / Rust **92bf873487ebdd7736aa30c22fba63bb10c3c1d7**。status/index空、无构建，已开放经理整合窗口，源码冻结。

## 原始失败与证据边界

只读C原始935 final-attempt1元数据及test日志：check0，test cargo/build.ps1 101、wrapper1；lib199通过1失败，后续integration与fmt/clippy/wasm未执行。w06_nonzero_and_timeout_are_unknown_after_spawn末尾assert_pid_absent对数字PID41068另spawn pwsh查询，code9。原日志没有创建时间/原句柄、无法判定循环哪一例。对应C[停止报告](../../../员工C/提交报告/第十四轮/员工C（935a27c最终五门禁attempt1停止报告）.md)。

**原935 Some9的具体原因仍未证实。** 不能倒写为已证实PID复用，也不能据这一次日志证明生产kill/wait失败。原实现同源8线程限定组件20/0未复现，不表示原最终失败消失。经理允许有限诊断后改进测试观测，不无限随机重跑。

测试构建初次增加原进程句柄/创建时间与查询命中信息后，trace-attempt2出现0通过3失败code1：这是新增诊断脚本把Get-Process缺失后的退出语义改变所致，和原code9不同；显式exit0恢复原absence语义。该新失败原件完整保留，不能算原D08重现。此轮日志原句柄signal0、kill/wait成功；修正后trace-attempt3三个W06通过，所有对应原句柄已终止，未得到原PID41068存活/复用的历史证明。

## 修订内容

仅cfg(test,windows)捕获tokio Child的原始进程handle，安全复制为OwnedHandle并取得创建FILETIME；保留副本直到fixture最终断言。Windows存活判定改为WaitForSingleObject(原handle,0)必须返回已终止。数字PID只作为本次fixture标记与原句柄记录关联键；缺失记录直接失败，不接受仅数字PID全局查询或一律放行。句柄保留也避免该原进程身份在断言前被数字重用掩盖。

真实正反控启动同一进程：仍活时require_original_process_exited返回Err；start_kill并5秒内wait完成后同一检查返回Ok。原W06 nonzero/timeout/overflow仍实际调用生产run，验证OutcomeUnknown、partial副作用保留、限时和真实原句柄终止；inherited pipe父进程同样原句柄验证。两例nonzero/timeout各自Fixture，避免上一例pid/partial文件被下一例误读，并输出deadline标签。

生产cleanup逻辑没有因未证实根因而改变：仍start_kill及同一个5秒timeout_at(child.wait)，错误结果/预算保持；仅将返回值绑定局部变量供cfg(test)记录，生产端继续丢弃。无生产环境注入开关或新公开API、无增加sleep或放宽存活断言。

**descendant限制保留**：继承pipe用例的子孙进程不由run直接spawn；测试在其pid文件已读后用OpenProcess留句柄，再由helper Kill+WaitForExit，最后检查该句柄。它是晚获取句柄，不能等同从最初spawn保留的身份链，也不声称OS级任意后代全灭。该测试helper清理职责及此限制没有用于证明生产全进程树清理。

## 验证与失败保留

| 原日志 | 固定状态 | 结果 |
|---|---|---|
| d08-original-attempt1 | 2a459，原935同源 | D组件8线程20/0；1helper，19实质 |
| d08-trace-attempt2 | f792788 | 0/3、101；新增诊断脚本code1失败，非原code9 |
| d08-trace-attempt3 | 191c163 | 三W06通过，原句柄signal0、kill/wait成功 |
| d08-handle-attempt4 | 404a789 | 四W06通过，含原句柄alive/terminated真实正反控 |
| d08-components-attempt5 | 527f466 | 八线程21通过0失败0忽略，1helper，20实质；180 filtered out |
| d08-clippy-attempt1 | 527f466 | 101，唯一C server.rs2646 collapsible_if，经理登记N13-C02；D不越权更改 |
| d08-fmt-attempt1 | 527f466 | 两owned文件fmt0 |

[14对原始/发布SHA256清单](evidence-0.5-manifest.json)完整保留所有新增失败，发布路径相对化原件不改；JSON记录full HEAD、两tree、dirty、source hash、起止、命令、退出码。未再跑全量测试或最终五门禁。C代码的Clippy问题不通过D反复测试解决，等待经理整合后由C唯一修订及新最终attempt2。

下一第一操作：等待经理对固定源码/证据review，保持源码和no-build冻结。最终APPROVED尚未获得，不能把21/0替代联合门禁。