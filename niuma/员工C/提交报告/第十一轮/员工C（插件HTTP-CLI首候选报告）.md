# C-R11-01 插件HTTP/CLI首候选报告

2026-09-27，员工C / GPT-6-astra，执行ID C-R11-01-20260927-E01。依据NEXT-10契约1.0及接口补充1，独立树 ../nhc-c-plugin-interface；Git提交C、跨树整合经理。

候选9bc2a4f2f6cfcd6ac1709053e7275dd4c62e1ae1，NoManCode tree446bd458b206aafdc36466ae09495adc0d95f9e3，Rust tree7f51c3ad427d3855101a29a81e03c736e79b7b35。基线fb6fe096a4ab3209002bfbb24c51872099530191，提交后status/index空。源码暂停供经理合真实B/D接口；本候选不能独立编译，禁止把未编译称通过。

唯一修改五文件：src/server.rs、src/cli.rs、tests/plugin_http.rs、tests/plugin_cli.rs、tests/http_contract.rs。main未改；旧HTTP fixture仅健康schema预期改SCHEMA_VERSION。

实现三条严格认证HTTP路由、插件业务错误独立固定安全映射；插件所有GET也bootstrap token。CLI list/status/set-enabled经本机HTTP，保留receipt/current分离；非收敛200先完整JSON输出stdout，再专用Failure sentinel使已有main exit1，print不生成二次错误对象。丢响应不重发，提示保留原key/原参数手动回放。

测试代码已准备：F02查询全表逻辑快照不变与实际内存停用状态；F06 CAS/异参/同状态新key/旧回放不协调/异key并发；F07数据库trigger拒绝不变runtime/不消耗key；F09匿名/跨源/重复字段/不合法类型/未知字段/size/content-type/key矩阵、实际CLI子进程完整响应/安全错误/丢响应/非收敛exit1。F05真实Host kill+wait、同库重启disabled保持、provider脚本拒绝read及显式enable后read正控。另真实Host接受命令后relay丢响应，原key手动CLI回放检查同receipt和全表无新增。上述均尚未执行。

已运行：stable rustfmt --edition 2024仅本人五文件，exit0；git diff --cached --check exit0。未运行：编译、定向测试、最终五门禁、付费live、release。无测试日志或双hash可发布，不能沿用NEXT-09证据。

环境：工程target兼容目录及本人树.local/tmp、target、logs已在活动盘准备；机器MSVC仅只读。全部工具显式活动E工作目录，未访问D旧仓、未操作共享index、无跨树merge/reset/clean/stash/push。人员档案仅主仓本人目录。

下一第一操作：经理合真实B/D API后核固定组合SHA/tree/status，运行plugin_http/plugin_cli定向，失败原日志保留并修复。最终F11仅经理冻结完整候选后C唯一串行。