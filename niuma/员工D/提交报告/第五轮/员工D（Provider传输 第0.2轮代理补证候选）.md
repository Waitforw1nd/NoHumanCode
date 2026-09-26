# D-R5-01 第0.2轮代理补证候选

2026-09-27，员工D/GPT-6-astra，执行ID不变。按经理第0.1只读意见补强同进程同loopback目标的环境代理正对照；候选3ac33d9af8ca2b31ca2cca7cfaff794474bfe8a0，相对43fc6ee仅provider.rs测试78增27删，status空。生产行为未改。

配对子进程先用受控Provider请求真实target，断言target一次且结果m；接着同timeout/redirect/retry配置、仅去掉no_proxy的reqwest client向同target发相同GET/bearer，期望proxy-sentinel响应、target次数不增。父进程每个配对子进程只允许proxy一次，并确认绝对loopback请求地址；另保留explicit proxy哨兵正对照。环境upper/lower及空/不匹配NO_PROXY分支在独立子进程执行，清除REQUEST_METHOD避免CGI规则干扰；Command.output等待子进程实际退出，listener abort后await。Windows环境变量名称不区分大小写，两个命名分支不宣称独立Windows解析语义。

只读锁定依赖reqwest0.12.28→hyper-util0.1.20源码确认Matcher::from_system先无条件from_env，system-proxy feature仅扩展OS配置；不是“禁default features就不读环境代理”。本次增加正对照用于实际验证，不能以源码推断代替通过证据。

单文件rustfmt和git diff --check实际退出0。编译/测试仍未执行，等经理串行合真实B/C依赖回D；不造stub、不公开permit构造器。下一操作核真实组合后运行定向，任何失败原样留证。历史第0.1报告保留。
