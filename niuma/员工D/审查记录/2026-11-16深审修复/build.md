# 构建门禁与交叉审查

2026-11-16，项目经理。实现 `1b0a09b82ff268679b86e7910cea1effc39f7ddc`。结论：APPROVED（构建产物选择及验证入口）。

build从本次Cargo消息选择当前Host源码对应的唯一二进制，支持自定义绝对/相对target与target triple；失败、缺失、歧义、错误包、文件占用均不误发默认旧产物。临时文件同目录替换，失败清理本次暂存。日常test在Rust通过后运行网页行为验证，独立web入口无需MSVC。minimal工具链显式包含rustfmt/clippy，未更新版本/依赖或改动MSVC发现逻辑。

经理独立复跑13个mock构建/门禁场景及6组网页断言，均通过。固定组合实际debug程序构建也通过；未执行release分发，因此mock不得描述为实际release产物验收。新增test入口需要Node，README已说明。

D交叉审找出安全流中的字段优先级、长分隔符、空值、持久正文和二次delta过滤反例，原反例保留于其报告；最终修复由A/B实现，经理10项安全与1项可信delta回归通过。D本人没有运行Cargo或越权改动A/B文件。

D追加检查经理本地门禁/证据脚本，指出最终成功manifest写入顺序和发布前原日志哈希核对缺口。经理将新版runner的最终成功记录放到tree/dirty核查之后；publisher在当前产品干净且双tree匹配的前提下，逐项核对已登记日志的大小与SHA，并缓存同一bytes供发布和统计。D于 `b042359a1f09877e512b62273592f425c0b09f73` 静态复审闭合D-EV-01/02。实际attempt2运行使用旧runner，原脚本保留，由发布时独立检查补充验收，不能宣称新版runner已在该次运行使用。
