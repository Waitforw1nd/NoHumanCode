# 诊断样例（D-R1-01 证据）

以下样例均为 `parse_manifest` + `PluginCatalog::register` + `resolve` 的公开 API 行为，
对应的集成测试断言见 `tests/plugin_catalog.rs`（D02/D03/D06/D10）。

## 1. 版本不符（VersionMismatch）

输入：`demo.consumer` 要求 `service/greeter` **2.0.0**，目录中只有 `demo.provider` 提供 `service/greeter` 1.0.0。

`resolve(["demo.consumer"])` 返回：

```
CatalogError::VersionMismatch {
    consumer_id: "demo.consumer",
    requirement: service/greeter/2.0.0,
    available: [1.0.0],
}
```

区别于 MissingDependency：同 kind+name 的提供者存在，只是版本不符；`available` 为升序去重的可用版本。

## 2. 真实闭环（DependencyCycle）

输入：`pa` requires `service/from-b`（由 `pb` 提供），`pb` requires `service/from-a`（由 `pa` 提供），另有环外节点 `x` requires `service/from-a`（非环成员）。对应 `d05_cycles_report_real_closed_paths_only`。

`resolve(["pa"])` / `resolve(["x"])` 均返回：

```
CatalogError::DependencyCycle { path: ["pa", "pb", "pa"] }
```

path 首尾相同、每一跳都是 consumer→provider 真实边；`x` 不出现在环路径中。
另有 `loopy` 自环样例返回 `path: ["loopy", "loopy"]`。

## 3. 多提供者歧义（AmbiguousProvider）

输入：两个插件分别在不同版本声明同一精确接口 `service/s/1.0.0`（id 不同）。

`resolve` 返回 `AmbiguousProvider { providers: [...] }`，providers 按 id 升序；解析不按显示名或注册顺序挑提供者。

## 4. 缺失依赖（MissingDependency）

输入：root 要求某接口，目录中无任何同 kind+name 提供者。

`resolve` 返回 `MissingDependency { consumer_id, requirement }`；`requires` 内未满足声明在注册时不拦截（register 只校验声明合法性）。
