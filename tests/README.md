# 验收测试

不把 Key 写进命令历史或文件时，可以在当前 PowerShell 会话临时设置两个环境变量，再运行真实的双 Key 并行检查：

```powershell
$env:PEACHSH_TEST_KEY_1 = Read-Host 'Key 1'
$env:PEACHSH_TEST_KEY_2 = Read-Host 'Key 2'
node D:\peachsh-harness\tests\test-openai-parallel.mjs
Remove-Item Env:PEACHSH_TEST_KEY_1,Env:PEACHSH_TEST_KEY_2
```

测试只输出 HTTP 状态、耗时和模型名，不输出 Key 或模型回复正文。
