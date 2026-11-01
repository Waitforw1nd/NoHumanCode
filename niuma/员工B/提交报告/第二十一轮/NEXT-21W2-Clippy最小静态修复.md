# NEXT-21W2 Clippy 最小静态修复

2026-11-02 员工B。经理核验 f898 attempt3 的 workspace 串行测试原 exit 0（64块、849 passed、0 failed、25 ignored）、fmt exit 0 和 Clippy 5项 exit 101，并授权原B树最小修复；上述是旧候选事实，不是新候选实测。旧 attempt3 48件原件/锁副本重新双hash核验零差异，清单 SHA256 `37baf313be64fe5aad9b3cf35448b809647bd52c655a35f36472d80062cf807e`；原锁 `1913b9fa7a99ed3b0c7c5d60111308509f329d15014fd92a78a453b1b40757f4` 原位保留，旧prepared manifest `9cc4dba82ac2a63d18e4ab44e79949e4a2f079cb10ec3b8f72dbafe9817cad31` 不变。

候选基线 `f898f0b372bada37e2b962890c3dca23935c2f40`，原B独立树分支 `codex/b/wasm-worker-process`，员工B提交候选 `a4366b56cc88644336d45f961313f294b05ea5ec`，产品 tree `4935f1aa104de548c97a26043cb0d5e442e24dea`、Rust tree `bfea730396003f4aa675cc25dcfaa1c6b39f1e14`。仅两文件、7增8删，独立树 clean/index空；经理为整合人，主仓index未操作。

- `NoManCode/rust-app/src/wasm_worker_process.rs` SHA256 `2b2984015722eec3696bdb85d332ceb3bff9d9c55fd171fb724718465f52589d`。
- `NoManCode/rust-app/src/wasm_worker_process_tests.rs` SHA256 `8d714c7a88d54db8b88d5cc5d77e77d361d3f2581ef2dea6a6fbf799999eab7b`。

W06测试helper的3个 SECURITY_ATTRIBUTES 输入引用与W06 sentinel的1个 attributes 输入引用改共享引用，两个不再需要可变声明；W05同一短路条件在原处用 `&& let Some(phase) = later` 绑定，维持原 ack 判断、阶段值、停点读取及断言。没有新增 allow；未改生产逻辑、协议、预算或断言。静态验证 `rustfmt --edition 2024 --check` 两文件 exit0，`git diff --check f898 HEAD` exit0。未运行 Cargo、Clippy、测试、worker/probe或新的受控进程；新候选尚无动态证据。

## 前置门禁静态方案，未创建runner或运行

旧prepared含固定 f898，不能拿来冒充新候选。经理审候选后，可另固定新外围脚本副本，绑定本候选HEAD和双tree、完整跟踪文件 before/after、clean/index、已审原句柄监督/未知持有机制，并保留旧runner；须另审其hash。独立新现场和TMP，使用同一已核B target与显式已安装MSVC/SDK入口、offline/locked、jobs2/debug0/incremental0。首两步按顺序，超时各1200秒、首个非零或原对象状态unknown立即停止并保留锁、句柄和原件：

1. `cargo check --workspace --all-targets --offline --locked --target-dir <B缓存>`
2. `cargo clippy --workspace --all-targets --offline --locked --target-dir <B缓存> -- -D warnings`

前置通过仅证明这两项，不取代经理随后安排的新候选矩阵、完整workspace及其他最终门禁；运行授权、旧锁处置、runner固定和签窗均待经理另定。

## 相对 f898 的完整 diff

```diff
diff --git a/NoManCode/rust-app/src/wasm_worker_process.rs b/NoManCode/rust-app/src/wasm_worker_process.rs
index efd709bb..4e47e3bf 100644
--- a/NoManCode/rust-app/src/wasm_worker_process.rs
+++ b/NoManCode/rust-app/src/wasm_worker_process.rs
@@ -1726,12 +1726,12 @@ mod platform {
         #[test]
         #[ignore = "requires controlled isolated process or artifact fixture"]
         fn sentinel_explicit_inheritance_control() {
-            let mut security = SECURITY_ATTRIBUTES {
+            let security = SECURITY_ATTRIBUTES {
                 nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                 lpSecurityDescriptor: null_mut(),
                 bInheritHandle: 1,
             };
-            let event = unsafe { own(CreateEventW(&mut security, 1, 0, null()), "positive_event") }
+            let event = unsafe { own(CreateEventW(&security, 1, 0, null()), "positive_event") }
                 .expect("positive sentinel");
             let mut event_flags = 0;
             assert_ne!(
@@ -1748,7 +1748,7 @@ mod platform {
             let mut parent_in = null_mut();
             let mut parent_out = null_mut();
             let mut child_out = null_mut();
-            if unsafe { CreatePipe(&mut child_in, &mut parent_in, &mut security, 0) } == 0 {
+            if unsafe { CreatePipe(&mut child_in, &mut parent_in, &security, 0) } == 0 {
                 panic!(
                     "W06 positive stdin pipe not established: {:?}",
                     std::io::Error::last_os_error().raw_os_error()
@@ -1756,7 +1756,7 @@ mod platform {
             }
             let child_in = unsafe { OwnedHandle::from_raw_handle(child_in) };
             let parent_in = unsafe { OwnedHandle::from_raw_handle(parent_in) };
-            if unsafe { CreatePipe(&mut parent_out, &mut child_out, &mut security, 0) } == 0 {
+            if unsafe { CreatePipe(&mut parent_out, &mut child_out, &security, 0) } == 0 {
                 panic!(
                     "W06 positive stdout pipe not established: {:?}",
                     std::io::Error::last_os_error().raw_os_error()
diff --git a/NoManCode/rust-app/src/wasm_worker_process_tests.rs b/NoManCode/rust-app/src/wasm_worker_process_tests.rs
index 828670e7..33a5b2ff 100644
--- a/NoManCode/rust-app/src/wasm_worker_process_tests.rs
+++ b/NoManCode/rust-app/src/wasm_worker_process_tests.rs
@@ -1117,12 +1117,12 @@ mod fault_matrix {
                 PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
             },
         };
-        let mut attributes = SECURITY_ATTRIBUTES {
+        let attributes = SECURITY_ATTRIBUTES {
             nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
             lpSecurityDescriptor: std::ptr::null_mut(),
             bInheritHandle: 1,
         };
-        let event_raw = unsafe { CreateEventW(&mut attributes, 1, 0, std::ptr::null()) };
+        let event_raw = unsafe { CreateEventW(&attributes, 1, 0, std::ptr::null()) };
         assert!(
             !event_raw.is_null(),
             "create sentinel: {:?}",
@@ -1540,9 +1540,8 @@ mod fault_matrix {
                             && v["kind"] == "advance"
                             && v["helper_pid"].as_u64() == Some(helper_pid as u64)
                     })
-                    && later.is_some()
+                    && let Some(phase) = later
                 {
-                    let phase = later.unwrap();
                     test_hooks::release_with_next(phase);
                     let reached =
                         wait_until(|| test_hooks::phase_reached(phase), Duration::from_secs(3))
```
