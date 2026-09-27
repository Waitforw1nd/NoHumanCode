# D-R8-01 第0.1轮可编译API候选

2026-09-27，员工D GPT-6-astra，执行ID D-R8-01-20260927-E01。经理明确新派发，源码在独立树 ../nhc-d-plugin-control，分支 codex/d/plugin-control；本人提交，仅经理跨树整合。档案仅主仓niuma。

基线fb6fe096a4ab3209002bfbb24c51872099530191；本候选3bda832de270d5928cbbada441e537e7ea7dcb2b，NoManCode tree37d340012387c9f268a340cedf6ac1462e41b154，Rust tree5cc834f6c37bb3dc2ac008259eb54aacc3f021e5。唯一源码src/tool_runtime.rs；提交后status/index空。

API已获经理接受：pub(crate) with_files_control<R>(&self, f: impl FnOnce(&mut FilesControl<'_>)->anyhow::Result<R>)->Result<R>；FilesControl::snapshot(&self)->FilesRuntimeStatus，reconcile(&mut self,bool)->FilesRuntimeStatus；pub builtin_with_enabled(bool)->Result<Self>。生命周期失败返回真实failed状态，不冒称此前Store commit未接受；锁/Store callback错误仍外传。

FilesRuntimeStatus公开字段：state:FilesRuntimeState(Active/Stopped/Unloaded/Failed)，admission_open:bool、in_flight:u64、draining:bool；instances:Vec<FilesInstanceStatus>{plugin_id:String,state:FilesInstanceState(Registered/Loaded/Active/Stopped/Unloaded/Failed),failure_stage:Option<FilesFailureStage(Activate/Deactivate/Deliver)>}；providers:Vec<FilesProviderStatus>{key:InterfaceKey,provider_id:String}；effects_count/subscriptions_count:u64；recovery:Vec<FilesRecoveryStatus>{plugin_id:String,stage:FilesFailureStage,cause_code:FilesFailureCauseCode(PluginError/Panic/ProvidesNotCovered)}。所有DTO双向serde，枚举snake_case。

false构造不激活，snapshot明确两个unloaded实例；管理/acquire同锁，关闭后不调用admit。lease单次私有RAII AtomicU64，失败claim不计数，Drop不取runtime锁。旧disable/unload仅内存兼容。失败重建先停止残留再unload/start，保留Host recovery；Host激活失败未自动写recovery，运行时补安全cause记录，不公开message。

本候选实际验证：pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action check，exit0；git diff --check及cached --check exit0。原始UTC/命令/exit在独立树.local/component-check-01.json，原始日志.local/component-check-01.log，后续正式组件报告发布双hash。TMP/TEMP、target及日志只E树.local，现有MSVC只读，未安装更新。第三方proc-macro-error2未来兼容提示保留。

F02/F04/F07/F10测试仍未执行；本报告仅可编译接口，不是功能验收。继续补限定组件测试和失败/计数反例；最终五门禁仅C。没有重跑NEXT-09、release/live/push/跨树merge或共享index。
