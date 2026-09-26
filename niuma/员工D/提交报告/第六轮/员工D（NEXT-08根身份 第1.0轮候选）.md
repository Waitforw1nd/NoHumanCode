# 员工D NEXT-08 根身份 第1.0轮候选

2026-09-27，员工D GPT-6-astra；D-R6-01-20260927-E01。经理直接派发，状态为候选待review，不自称APPROVED。

基线80480c90fba51ec8aeadc01b2d8f184ae0bd1d5a；独立树相对活动仓库为 ../nhc-d-workspace-identity，分支codex/d/workspace-identity。本人精确暂存提交源码，经理唯一整合。共享档案只写活动主仓本人目录，未编辑独立树档案。

固定候选3452c67f6734f8ec2d67addd7ba2a4ddf3f7d317；NoManCode tree 411179e50e749a6531202b7f58e3208de3aa5c3c；Rust tree ce52cc78df58993c7ec30771d3b4a230532e2a59；提交后git status --short空。源码暂停供经理review/串行合给B/C。

## 交付范围

精确三文件：NoManCode/rust-app/src/workspace_identity.rs、tests/workspace_identity.rs、Cargo.toml。Cargo仅windows-sys Win32_Storage_FileSystem feature；--locked成功，Cargo.lock无需变化。lib归B，测试通过path导入本人模块。不改Workspace/Engine/Store。

RootIdentity::capture/verify/overlap、canonical_path getter、VerifiedRoot::path、RootOverlap四值和verify_directory_policy按契约§9实现。RootIdentity支持Clone/Debug/Eq/serde严格字段。Windows使用FileIdInfo卷+128位file ID、FileAttributeTagInfo和FileCaseSensitiveInfo；后者结构windows-sys 0.59 Win32未导出，按Windows DWORD Flags ABI局部repr(C)定义。只接受普通盘符及自身规范长路径，拒UNC/设备前缀、非目录、相对路径等。

输入路径与canonical路径逐祖先核reparse/case-sensitive并前后两遍捕获。overlap验证两根并再次复核祖先链，按真实目录ID判Same/包含；任一失效不当Disjoint。verify_directory_policy用于B拒大小写敏感子目录，不赋Task权限、不代替既有叶节点和越界检查。

错误准确变体：InvalidPath、UnsupportedPlatform、UnsupportedIdentity、ReparsePoint、CaseSensitiveDirectory、Unavailable(io::Error)、IdentityChanged、InvalidSnapshot。Display固定安全文本，Unavailable保留source供内部诊断。副作用调用方仍负责临执行验证；VerifiedRoot不是长期授权租约，不承诺handle隔离或消灭TOCTOU。

## 实测与失败记录

Rust工作目录为独立树NoManCode/rust-app。第一次直接cargo test因shell未配置MSVC，link.exe not found，exit101，未到模块源码编译；原始日志test-01保留。随后根据已读build.ps1环境初始化段设置实际VS安装并初始化MSVC/SDK，未改build脚本；进程环境CARGO_PROFILE_DEV_DEBUG=0、CARGO_PROFILE_TEST_DEBUG=0、CARGO_INCREMENTAL=0。

| 日志 | 命令 | exit / 结论 |
| --- | --- | --- |
| d-r6-test-01 | cargo test --locked --test workspace_identity -- --nocapture | 101；MSVC环境失败 |
| d-r6-test-02 | 同上，正确MSVC环境 | 0；8/0/0，中间候选 |
| d-r6-test-03 | 同上，增加真实删除重建/坏ID | 0；9/0/0，中间候选 |
| d-r6-test-04 | 同上，删除重建用例明确Windows条件 | 0；最终9/0/0 |
| d-r6-clippy-01 | cargo clippy --locked --test workspace_identity -- -D warnings | 0，中间候选 |
| d-r6-clippy-02 | 同上 | 0，最终候选 |
| d-r6-fmt-01 / 02 | rustfmt --edition 2024 --check src/workspace_identity.rs tests/workspace_identity.rs | 均0；02最终候选 |
| d-r6-diff-01 | git diff --check | 0；最终提交前再执行亦0 |

9项真实Windows测试覆盖：非ASCII/serde往返/双向嵌套与同根及兄弟；缺失/移动替换；真实删除重建；坏版本/平台/字段/路径/ID；双方不存在；根与祖先junction；已有根换junction；真实fsutil大小写敏感根与子目录策略；大小写别名/规范长路径/设备与UNC输入。junction和fsutil fixture成功建立，不跳过，不改系统全局功能。UnixFixture使用真实dev/ino并有symlink反例，Windows不接受UnixFixture snapshot；本轮未在Unix执行，不冒称Unix通过，也不把Unix dev/ino当Windows证据。

定向Clippy会编译现有lib依赖，但不是最终workspace五门禁。未执行全workspace test/check、wasm-check、release、live或push。经理/C负责固定组合正式门禁。

## 证据与后续

[日志双hash](日志双hash.json)逐项列出原始与发布副本SHA256。原始日志保留首次失败和中间轮次；发布副本仅将独立树绝对根替换为相对位置，不改测试事实。原始与发布在本轮独立子目录，未覆写旧报告。

当前无模块阻塞。下一第一操作：经理对固定SHA review，串行合入B/C依赖；D等待明确返工或只读接入review，不继续修改候选。项目级RootIdentity sidecar/legacy_unbound、权限准入/撤权、并发排他与API接入均由B/C继续，不属于D本组件通过所证明的范围。