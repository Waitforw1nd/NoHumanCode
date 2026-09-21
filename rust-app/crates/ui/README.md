# `peachsh-ui`

这是 🍑sh 的 Leptos CSR/WASM 迁移壳。它只渲染 UI 并依赖 `peachsh-protocol`；Provider Key、DPAPI、文件、进程和 SQLite 仍由 Rust Host 管理。

当前仅包含可编译的对话页面骨架。Host 仍提供旧 HTML/JavaScript 回退，后续按“项目 → 对话 → 审批 → diff → 恢复”的垂直切片迁移页面。

验证：

```powershell
cargo check -p peachsh-ui
# 在 rust-app 目录执行，脚本会先加载 Windows 链接器环境
.\build.ps1 -Action wasm-check
```
