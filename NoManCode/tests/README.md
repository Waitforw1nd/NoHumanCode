# 验收测试

从仓库根执行。经授权需要真实双 Key 并行检查时，可以在当前 PowerShell 会话临时设置两个环境变量：

```powershell
$env:PEACHSH_TEST_KEY_1 = Read-Host 'Key 1'
$env:PEACHSH_TEST_KEY_2 = Read-Host 'Key 2'
node ./NoManCode/tests/test-openai-parallel.mjs
Remove-Item Env:PEACHSH_TEST_KEY_1,Env:PEACHSH_TEST_KEY_2
```

测试只输出 HTTP 状态、耗时和模型名，不输出 Key 或模型回复正文。
