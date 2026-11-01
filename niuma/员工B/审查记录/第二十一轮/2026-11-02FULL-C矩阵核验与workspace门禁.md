# FULL-C矩阵核验与workspace门禁

2026-11-02，项目经理。受测f898f0b372bada37e2b962890c3dca23935c2f40，产品77d0ecdf81b4ad90029bbc85db06271cc4478276，Rust43a9b604fb5e21b111cc831d98d2a92794d0962f。经理读取22份case状态（list+21exact）、资源/CPU/W04双分支和W05/W06全部stdout及stderr、source-final/manifest/window-result。build与lib-no-run退出0，21exact各1pass/0fail，UTC16:46:09.6110358至16:49:14.2099232，outer exit0/completed、gate不存在，source-final clean/index空。

本次独立固定件SHA256：worker05d1a4b1c8c089f175ec316001036723930ba8dbdd4e7fb26ffca415f0770691，probe6691e01d920e457d942d5a3e17883ea55e35a0dbbeb056c10d139bc530c70e6a，lib-test6b98bbaa4313d0d5e72b6ed13c7847858e2a41a6f7262ad62e86d6dd560ca38d。经理已独立核1200份固定/逐case EXE与218份发布原件，逐字节/长度/双hash零差异；发布814626字节，清单SHA256 18eadb548258a5096c878d1159eb94cd49252a9ec3294602b90895d159c549e6。审计JSON为经理审查记录/2026-11-02FULL-C二十一项与工件核验.json、2026-11-02FULL-C发布证据核验.json。

资源忙probe48192实际用户CPU30781250 ticks、采样wait258、Host预算门赢，941ms返回worker_cpu_budget并收束；最终有效响应78372的30625000 ticks被最终门拒绝。查询失败及取消/截止竞争exact通过，不能外推硬实时3秒上限。W04无效flags真实87，普通create_after_ticket本次真实5，create_returned_suspended真实PID57112回收；不声称本轮在create_after_ticket观察到late success，也不反写旧249479根因。

W05 before/suspended/resumed/pending均在截止前定向终止原helper，实际三个worker的原观察句柄均wait0（77232/76936/49520），before按停点证明未发创建，不用缺PID推断。W06当前P01观测通过；写映射已建立后pin拒绝32；实际映像反例/事件同对象显式继承正控/事件及Job非继承负控通过；active2允许真实挂起子进程并回收、active1拒1816，breakaway允许时实际不在Job且回收、禁用拒5。R04真实Job active0、原process signal、无watchdog代劳。以上限定于当前测试装配与威胁边界，不把WASM信任工件等同任意native隔离或生产绑定。

## 下一唯一门禁窗口

准许原B在相同固定f898候选、全新B21-W2-GATES-A现场，按以下六步串行完成，精确命令和实际退出码/UTC/源码前后及工件来源留证：workspace all-targets check；显式debug Host bin peachsh build；完整workspace test；cargo fmt --all -- --check；workspace all-targets Clippy -D warnings；已安装wasm32-unknown-unknown的peachsh-ui check。均offline/locked（fmt按原命令），E活动与原环境保持，不下载/升级工具链、不release/live/push。新target可复用B自身缓存，旧FULL-C固定副本不得覆盖。准备本树旧fixture所需target目录仅在E、不得删除旧现场。

为执行当前真实进程串行要求，workspace test显式-- --test-threads=1并在报告标明；保留用例内并发/竞争断言，不将本次串行结果写成默认并发实跑。W2 ignored21和3 helper不得在普通workspace里去掉ignore再额外跑；本轮不重复已通过21矩阵。每个门禁有界受控、首个非零或进程状态不明停下保留原件，不启动后续步骤，不按名称杀进程。编译/lint/测试失败先回报固定新候选与具体差异，再决定后续验证范围。

这只是矩阵范围核验与后续门禁授权，不是W2最终联合验收、主线整合或项目完成。C继续完整NEXT-21U静态，D冻结。

## GATES-A前置停窗与接续

UTC16:58:03至16:58:04在环境探测前置遇外围StrictMode错误，current=null，六步均未启动。经理读取原start-gates.ps1、监督C#、incident/window-result及员工报告；不把外围自写exit_code当原外层句柄终态。原锁SHA256 496581d74aea962d0f25f92bca98cb6622cb3f83a3544bba96ef1b1f1db228f1保留。要求B在新目录准备修订并保全旧现场：环境探测局部关闭StrictMode、避免$Host常量冲突、实际隐藏flags与报告一致、直接原句柄Wait/GetExitCode及未知时原观察对象持有。仅静态准备，重型六步待经理固定审查再签。源码f898不变，FULL-C有效范围不变。
