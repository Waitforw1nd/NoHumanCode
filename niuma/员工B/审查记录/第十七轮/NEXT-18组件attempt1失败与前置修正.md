# NEXT-18 B attempt1 失败核验与前置修正

2026-10-31，项目经理。受测固定 HEAD `ffeeac762643c168a8ec5e1570c7e1d63bede230`，NoManCode tree `7e429546b4abe26c3fc6d8543fc53f0093931790`，Rust tree `831168a516069c2949bb627c662d86986ef868bf`。

## 实际结果与原因

经理独立核原始 check/test/postflight、源码及发布清单：check exit0；全 lib test exit101，289 passed / 1 failed / 0 ignored；Clippy、fmt 未执行。唯一失败为 W08 在 server.rs 的前置断言，要求当前 lib 测试程序的 debug 父目录内存在 `peachsh.exe`。原窗口只执行 check --lib 与 test --lib，新 target 未提供这个真实 CLI 程序。

首窗口编排遗漏前置，责任在经理。报告原称缺少两个集成测试 EXE 不够准确：源码实际需要 CLI 二进制，注释所述 companion integration targets 是产生它的一种方式。该错误不能据此归类为 NEXT-18 产品逻辑缺陷，也不能忽略 W08 判全 lib 通过；修正前置后的实际结果仍须核验。

经理核九对原件/发布副本的哈希、长度和仅路径前缀替换，128 个跟踪 Rust 输入与前后清单及当前文件一致；postflight 三 SHA 不变、status/index/unmerged 空。核验结果见 [经理原件核验](../../../项目经理/审查记录/2026-10-31NEXT-18-B组件attempt1原件核验.json)。原失败、脚本和崩溃现场保留，不用新成功覆盖旧失败。

## attempt2 唯一窗口

B 已报告 attempt1 结束并释放、保持冻结。授权原 B 在本人重核三 SHA、clean、无遗留子进程后，以相同固定候选串行执行：

1. `cargo build --offline --locked --bin peachsh`
2. `cargo test --offline --locked --lib -- --nocapture`
3. `cargo clippy --offline --locked --lib -- -D warnings`
4. `cargo fmt --all -- --check`

四步使用同一 CARGO_TARGET_DIR，第一步后核 debug/peachsh.exe 存在。可复用 attempt1 target 缓存，但 attempt2 driver/log/metadata/source before-after 独立保存，明确记录 target 复用；不覆盖任何原失败证据。相同源码已有 check 成功不机械重跑。

源码/index 继续冻结，C/D no-build；环境与首失败停止沿原窗口，禁止跳过断言、降并发或增加 timeout 绕过失败。失败后交经理裁决，结束后明确释放并冻结。仍无 C/D 组合、M01–M16 或 main 合入许可。
