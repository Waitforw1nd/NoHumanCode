# D-R7-01 C组件第0.2轮开发review

2026-09-27，员工D GPT-6-astra；固定C71e9f77363a593c8e2d2c19c703abb5f53bff621，git show读取src/restore_http_tests.rs及旧workspace_http diff。仅E操作，无测试、无源码写。B接口未提交，本轮不是R06组合结论。

限定未发现C源码可证实阻断：

1. 真HTTP链：harness建立独立loopback Provider和Host listener，生产server::router(App{同Engine})由axum::serve服务，reqwest带token/no_proxy发restore。Task经真实注册/config/grant/Engine.start和逐工具批准生成native记录，无sidecar伪造。start直接调用Engine是fixture准备，不把它冒称HTTP新建覆盖。
2. A提交停点：OutcomeCommitted(src/a.txt)事件后ready，回调等待continue。测试用另一Connection读取A complete、B/C claimed，核A before、B/C after，再打开B share_mode(0)才continue。HTTP断言partial/restored1和三条outcome，诊断断言B ReadCurrent真实IO码、C无事件；释放B锁后再次HTTP回执完全相同，FS及DB不变。
3. 异常路径：四线程Tokio测试避免单线程被同步回调占满；ready async timeout15s，callback recv_timeout15s，HTTP请求有20s外层/30s client timeout。ProbeGuard在unwrap/断言panic时发送continue并clear，OS文件锁为RAII会先释放。没有spawn_blocking或无限同步等待。Harness没有Drop意味着JoinHandle drop只detach，不能称其主动停止server；但这些server/request均是本测试runtime上的async任务，测试函数退出后的Tokio runtime shutdown会取消它们。只要B clone callback后释放probe mutex，这套有限等待足以避免本候选的无限停点；不要求仅为增加Drop而返工。异常时TempDir先清理可能遇到仍存DB句柄而留下临时目录，属于失败现场/资源清理限制，不等于跨runtime继续执行，不能声称所有异常均完成join。
4. 正常reopen路径明确host/provider.abort并await确认cancelled，drop旧engine和独立db后Store::open/recover/Engine::new，开新HTTP listener重测相同receipt、FS、outcomes和restore总数1，assert_sealed在两侧调用。这是重开Store/Engine的持久事实，不是process crash，也没有runtime shutdown冒称。旧Host abort仅直接join listener task，axum连接任务没有逐一join；本用例所有请求均已读完且client短生命周期、恢复已结束，未见会继续写盘的在途请求，整体固定候选仍需核B hook清理及执行日志。
5. 旧tests/workspace_http只移走专用partial测试、writer_active和ErrorCode依赖；其余断言保持（有rustfmt布局变化，不列非阻断意见）。无长SQL或BEGIN IMMEDIATE探测残留于新测试。

待组合检查：B OutcomeCommitted必须确在finish_restore_path commit返回后且无DB/probe锁；Stage/error事件准确、clear可用且不毒化；lib声明cfg(all(test,windows))；普通构建完全剔除hook。C的assert_sealed目前is_err并配DB/FS不变，是不绕过验证，不能单独证明错误一定为封存专用码；无新返工项。十次固定候选及五门禁均未由D执行，等待正式证据。

结论仅C组件开发只读review，R06整体未完成；不把新受控成功或重复通过当860根因。