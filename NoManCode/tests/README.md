# 验收测试

日常门禁从 `NoManCode` 执行 `npm test`：先运行 Rust workspace 测试，成功后运行实际网页的 `rust-app/tests/project-web.mjs`；任一步失败返回该步退出码。兼容入口 `npm run test:rust` 使用同一门禁。`npm run test:web` 只执行网页 mock DOM/fetch 行为测试，不联网、不需要 Cargo；它不替代浏览器渲染验收或 Leptos WASM 编译检查。

只有 Node/PowerShell、没有 npm 的环境可从仓库根直接执行 `pwsh -NoProfile -File NoManCode/rust-app/build.ps1 -Action test`；轻量网页检查使用同脚本的 `-Action web-test`。无需为这些入口安装 Node 包。

构建脚本修改后运行 `npm run test:build-script`。该回归使用独立临时目录和 mock Cargo/Node，检查自定义输出目录、target triple、错误产物拒绝、失败保留既有分发及门禁退出码；不编译或启动产品。脚本打印保留证据位置。工具链门禁、受控进程矩阵与付费测试分别记录，普通测试不会自动运行 ignored 用例。

从仓库根执行。经授权需要真实双 Key 并行检查时，可以在当前 PowerShell 会话临时设置两个环境变量：

```powershell
$env:PEACHSH_TEST_KEY_1 = Read-Host 'Key 1'
$env:PEACHSH_TEST_KEY_2 = Read-Host 'Key 2'
node ./NoManCode/tests/test-openai-parallel.mjs
Remove-Item Env:PEACHSH_TEST_KEY_1,Env:PEACHSH_TEST_KEY_2
```

测试只输出 HTTP 状态、耗时和模型名，不输出 Key 或模型回复正文。
