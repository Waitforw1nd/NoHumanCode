# 🍑sh harness 配置

`peachsh.providers.yaml` 是多 Key 路由模板。把里面的 `llm-pi-ai.providers` 内容合并到 `data/settings.yaml`，再在 Harness 的凭据设置中分别填写 `PEACHSH_KEY_1`、`PEACHSH_KEY_2` 和 `PEACHSH_KEY_3`。

每个路由的 `baseURL` 当前指向 `https://xpeach.codes/v1`，默认模型示例为 `gpt-5.6-sol`。如果你的账户实际提供的是其他兼容地址或模型，只改这里的地址 / 模型 ID，不要把 Key 写入配置文件。

从仓库根在 PowerShell 中执行 `./NoManCode/set-peachsh-key.ps1 -Slot 1`、`-Slot 2` 或 `-Slot 3` 保存对应 Key。脚本使用 `DSH_HOME` 或默认 `NoManCode/data/`，需要先有可用的旧版凭据存储；只显示保存成功，不打印 Key，保存后重启相应实例。

如果需要确认站点实际的模型 ID，可以临时设置 `PEACHSH_DISCOVERY_KEY` 后从仓库根运行 `node ./NoManCode/tests/discover-peachsh-models.mjs`。收到 401 时优先检查 API Token 和 `/v1` 根地址，脚本不会把响应正文写入日志。

第一版采用手动路由绑定：队员通过 `spawn_teammate` 的 `llm_provider`、`model`、`reasoning_effort` 和 `max_tokens` 参数选择自己的路由和模型。没有指定这些参数的队员继续继承主代理配置。
