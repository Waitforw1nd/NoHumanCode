//! Small, deterministic WASM extension boundary for Peachsh.
//!
//! A plugin is a pure JSON transform. It receives UTF-8 JSON through linear
//! memory and returns a packed `(ptr << 32) | len` i64 from `run_json`. There
//! are no default imports, clocks, sockets, filesystem handles, or secrets.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{path::Path, time::Instant};
use wasmi::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

pub const ABI_VERSION: &str = "peachsh.wasm.v1";
pub const MAX_MODULE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_JSON_BYTES: usize = 512 * 1024;
pub const DEFAULT_FUEL: u64 = 5_000_000;
pub const MAX_FUEL: u64 = 50_000_000;
pub const MAX_MEMORY_PAGES: u64 = 64; // 4 MiB; enough for a transform, small enough for a local tool.
pub const MAX_TABLE_ELEMENTS: usize = 65_536;

struct WasmState {
    limits: StoreLimits,
}

fn store_limits(memory_pages: u64) -> StoreLimits {
    StoreLimitsBuilder::new()
        .memory_size((memory_pages.min(MAX_MEMORY_PAGES) * 65_536) as usize)
        .table_elements(MAX_TABLE_ELEMENTS)
        .instances(1)
        .tables(1)
        .memories(1)
        .build()
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub abi: String,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default = "default_fuel")]
    pub fuel: u64,
    #[serde(default = "default_memory_pages")]
    pub memory_pages: u64,
    pub sha256: String,
}
fn default_fuel() -> u64 {
    DEFAULT_FUEL
}
fn default_memory_pages() -> u64 {
    16
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Plugin {
    pub manifest: Manifest,
    pub wasm: Vec<u8>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct RunReport {
    pub plugin: String,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub fuel_left: u64,
    pub elapsed_ms: u128,
    pub output: Value,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn validate_manifest(manifest: &Manifest, wasm: &[u8]) -> Result<()> {
    ensure!(
        manifest.abi == ABI_VERSION,
        "WASM ABI 不兼容：{}",
        manifest.abi
    );
    ensure!(
        valid_id(&manifest.id),
        "WASM 插件 ID 必须是小写英文、数字和连字符"
    );
    ensure!(
        !manifest.name.trim().is_empty() && manifest.name.len() <= 200,
        "WASM 插件名称无效"
    );
    ensure!(
        !manifest.version.trim().is_empty() && manifest.version.len() <= 64,
        "WASM 插件版本无效"
    );
    ensure!(wasm.len() <= MAX_MODULE_BYTES, "WASM 模块超过 8 MiB");
    ensure!(
        (1..=MAX_FUEL).contains(&manifest.fuel),
        "WASM fuel 超出上限"
    );
    ensure!(
        (1..=MAX_MEMORY_PAGES).contains(&manifest.memory_pages),
        "WASM 内存超出上限"
    );
    ensure!(
        manifest
            .capabilities
            .iter()
            .all(|cap| matches!(cap.as_str(), "json")),
        "当前版本只允许 json capability"
    );
    ensure!(
        manifest.sha256.eq_ignore_ascii_case(&sha256_hex(wasm)),
        "WASM sha256 与 manifest 不匹配"
    );
    Ok(())
}

pub fn validate_module(wasm: &[u8], memory_pages: u64) -> Result<()> {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, wasm).context("WASM 模块解析失败")?;
    ensure!(
        module.imports().next().is_none(),
        "WASM 插件不能声明 host import；请通过 JSON ABI 工作"
    );
    ensure!(
        module.exports().any(|e| e.name() == "memory"),
        "WASM 插件必须导出 memory"
    );
    ensure!(
        module.exports().any(|e| e.name() == "alloc"),
        "WASM 插件必须导出 alloc(i32)->i32"
    );
    ensure!(
        module.exports().any(|e| e.name() == "run_json"),
        "WASM 插件必须导出 run_json(i32,i32)->i64"
    );
    let mut store = Store::new(
        &engine,
        WasmState {
            limits: store_limits(memory_pages),
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(1)?;
    let instance = Linker::<WasmState>::new(&engine).instantiate_and_start(&mut store, &module)?;
    let memory = instance
        .get_memory(&store, "memory")
        .context("WASM memory 无法读取")?;
    ensure!(
        memory.ty(&store).minimum() <= memory_pages && memory.size(&store) <= memory_pages,
        "WASM 初始内存超过 manifest 限制"
    );
    ensure!(
        memory
            .ty(&store)
            .maximum()
            .is_some_and(|max| max <= memory_pages && max <= MAX_MEMORY_PAGES),
        "WASM memory maximum 未限制在主机上限内"
    );
    Ok(())
}

pub fn run(plugin: &Plugin, input: &Value) -> Result<RunReport> {
    validate_manifest(&plugin.manifest, &plugin.wasm)?;
    validate_module(&plugin.wasm, plugin.manifest.memory_pages)?;
    let input_bytes = serde_json::to_vec(input)?;
    ensure!(input_bytes.len() <= MAX_JSON_BYTES, "WASM 输入超过 512 KiB");
    let started = Instant::now();
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &plugin.wasm).context("WASM 模块解析失败")?;
    let mut store = Store::new(
        &engine,
        WasmState {
            limits: store_limits(plugin.manifest.memory_pages),
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(plugin.manifest.fuel)?;
    let instance = Linker::<WasmState>::new(&engine).instantiate_and_start(&mut store, &module)?;
    let memory = instance
        .get_memory(&store, "memory")
        .context("WASM memory 缺失")?;
    let alloc = instance.get_typed_func::<i32, i32>(&store, "alloc")?;
    let run = instance.get_typed_func::<(i32, i32), i64>(&store, "run_json")?;
    let ptr = alloc.call(&mut store, input_bytes.len().try_into()?)?;
    ensure!(ptr >= 0, "WASM 返回了负的输入指针");
    memory.write(&mut store, ptr as usize, &input_bytes)?;
    let packed = run.call(&mut store, (ptr, input_bytes.len().try_into()?))? as u64;
    let output_ptr = (packed >> 32) as usize;
    let output_len = (packed & 0xffff_ffff) as usize;
    ensure!(output_len <= MAX_JSON_BYTES, "WASM 输出超过 512 KiB");
    let mut output_bytes = vec![0; output_len];
    memory.read(&store, output_ptr, &mut output_bytes)?;
    let output: Value = serde_json::from_slice(&output_bytes).context("WASM 返回的不是 JSON")?;
    Ok(RunReport {
        plugin: plugin.manifest.id.clone(),
        input_bytes: input_bytes.len(),
        output_bytes: output_bytes.len(),
        fuel_left: store.get_fuel()?,
        elapsed_ms: started.elapsed().as_millis(),
        output,
    })
}

pub fn load(directory: &Path, id: &str) -> Result<Plugin> {
    ensure!(valid_id(id), "插件 ID 无效");
    let root = directory.join(id);
    let manifest: Manifest = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json")).context("缺少 WASM manifest")?,
    )?;
    let wasm = std::fs::read(root.join("plugin.wasm")).context("缺少 WASM 模块")?;
    validate_manifest(&manifest, &wasm)?;
    validate_module(&wasm, manifest.memory_pages)?;
    Ok(Plugin { manifest, wasm })
}
pub fn list(directory: &Path) -> Result<Vec<Manifest>> {
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut result = vec![];
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir()
            && let Ok(plugin) = load(
                directory,
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default(),
            )
        {
            result.push(plugin.manifest);
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn echo_plugin() -> Plugin {
        let wat = r#"(module (memory (export "memory") 1 1) (func (export "alloc") (param i32) (result i32) (i32.const 0)) (func (export "run_json") (param i32 i32) (result i64) (i64.extend_i32_u (local.get 1))))"#;
        let wasm = wat::parse_str(wat).unwrap();
        let manifest = Manifest {
            abi: ABI_VERSION.into(),
            id: "echo".into(),
            name: "Echo".into(),
            version: "1".into(),
            capabilities: vec!["json".into()],
            fuel: DEFAULT_FUEL,
            memory_pages: 1,
            sha256: sha256_hex(&wasm),
        };
        Plugin { manifest, wasm }
    }
    #[test]
    fn echo_and_limits() {
        let plugin = echo_plugin();
        let report = run(&plugin, &serde_json::json!({"message":"你好"})).unwrap();
        assert_eq!(report.output["message"], "你好");
        assert!(report.fuel_left < DEFAULT_FUEL);
        let mut bad = plugin.clone();
        bad.manifest.capabilities = vec!["network".into()];
        assert!(run(&bad, &Value::Null).is_err());
    }
    #[test]
    fn manifest_rejects_imports() {
        let wat = r#"(module (import "host" "secret" (func)) (memory (export "memory") 1 1) (func (export "alloc") (param i32)(result i32)(i32.const 0)) (func (export "run_json") (param i32 i32)(result i64)(i64.const 0)))"#;
        let wasm = wat::parse_str(wat).unwrap();
        assert!(validate_module(&wasm, 1).is_err());
    }
}
