# WASM echo 示例

项目插件目录格式：

```text
.peachsh/plugins/echo/manifest.json
.peachsh/plugins/echo/plugin.wasm
```

`manifest.json` 示例：

```json
{
  "abi": "peachsh.wasm.v1",
  "id": "echo",
  "name": "Echo",
  "version": "1.0.0",
  "capabilities": ["json"],
  "fuel": 5000000,
  "memory_pages": 1,
  "sha256": "<plugin.wasm 的 SHA-256 小写十六进制>"
}
```

插件导出 `memory`、`alloc(i32) -> i32` 和 `run_json(i32, i32) -> i64`。宿主把 UTF-8 JSON 写入 `alloc` 返回的地址，再调用 `run_json`；返回值的高 32 位是输出地址，低 32 位是输出长度。插件不得声明 imports，也不能直接读取文件、访问网络或读取凭据。

Agent 成员开启项目工具后可以调用：

```json
{
  "plugin": "echo",
  "input": {"message": "hello"}
}
```

调用工具名为 `run_wasm`。同一插件也可以通过 `POST /api/wasm/run` 调用；运行前宿主会再次校验 manifest 和 SHA-256。

