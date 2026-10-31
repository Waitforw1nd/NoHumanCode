# NEXT-18 B 首候选静态审查与组件窗口

2026-10-31，项目经理。候选 `ffeeac762643c168a8ec5e1570c7e1d63bede230`，NoManCode `7e429546b4abe26c3fc6d8543fc53f0093931790`，Rust `831168a516069c2949bb627c662d86986ef868bf`。经理核HEAD/双tree、status/index/unmerged空，增量仅九授权文件；本人静态报告已读，diff空白检查通过。

结论：允许此固定版本进入B限定组件验证，未发现阻止该验证的静态问题。不等于M01–M16通过、C/D组合通过或允许合main。C273895d与D36b73d仍各自冻结/no-build，未合入B。

## 实现审查

复核固定差异及前期逐函数预读：schema16→17事务重建及严格旧/新分支、旧时间/rowid保全、四ID账本与旧公开白名单；新管理key回放先于Busy/CAS，本地状态仅在提交成功后变化；Agent任务lease持久准入、Provider两生产路径的revision和原网络/预算同事务claim；禁用构造、旧本地enable门控、锁外生命周期及guarded析构、permit提交后补偿。DTO与C当前校验关系一致，跨Store收敛仍只是采样。

九迁移停点、管理/准入停点与TEMP延迟FK故障均test-only；COMMIT观察标记取自真实SQLite FOREIGNKEY错误。B可释放barrier最终实现为按path/phase查找的Vec，D原单槽说明是较早草稿事实，保留已有测试guard不会降低原并发要求。真正故障行为、生命周期失败和历史夹具必须通过动态验证，不能只凭代码判断。

## 唯一组件窗口

执行人B，仍用原任务B-R17及独立树；本窗口仅解除构建禁令，源码/index保持冻结。本人开跑前再次核固定三SHA、clean和无其他重型构建。经理当前进程检查未见cargo/rustc/clippy-driver/peachsh匹配；不据此保证未来无进程，B开跑前仍核。

按顺序执行，首个失败即停止后续步骤，保存原件并报告，不用改断言/降并发/加timeout绕过失败：

1. `cargo check --offline --locked --lib`
2. `cargo test --offline --locked --lib -- --nocapture`
3. `cargo clippy --offline --locked --lib -- -D warnings`
4. `cargo fmt --all -- --check`

全lib测试用于本轮迁移、运行时和既有内部行为回归；C/D新增集成与独立模块不在该组合中，本窗口不能替代最终all-targets/wasm五门禁。未执行的步骤明确记未执行。失败后由经理依据实际错误交原owner修复并重新固定，不能原地改源码继续冒称同一候选。

沿现行环境：工程/target/TMP/TEMP/log在本E树，已有Rust/MSVC/SDK只读，offline/locked、DEV/TEST debug0、incremental0、jobs2，先创建旧fixture需要的rust-app/target。不得安装更新、付费live、release、push或清理。运行前后记录源码清单、完整SHA/tree/dirty状态、实际命令/UTC/退出码和原始日志；报告发布副本路径相对化时保留原件及逐字节/hash关系。窗口结束明确释放并持续冻结，供后续经理串行整合。
