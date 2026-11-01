//! Worker-only WASM execution. Do not call this parser from a Host process.
use crate::wasm_worker_protocol::{Decision, Error, Input, MAX_MODULE, MAX_RESPONSE, Manifest};
use sha2::{Digest, Sha256};
use wasmi::ValType;
use wasmi::{Config, Engine, ExternType, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

struct State {
    limits: StoreLimits,
}

pub fn execute(manifest: &Manifest, input: &Input, bytes: &[u8]) -> Result<Decision, Error> {
    manifest.validate()?;
    input.validate()?;
    if bytes.is_empty()
        || bytes.len() > MAX_MODULE
        || format!("{:x}", Sha256::digest(bytes)) != manifest.digest.to_ascii_lowercase()
    {
        return Err(Error::Manifest);
    }
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, bytes).map_err(|_| Error::Module)?;
    if module.imports().next().is_some() {
        return Err(Error::Module);
    }
    let mut memory_ok = false;
    let mut alloc_ok = false;
    let mut run_ok = false;
    for export in module.exports() {
        match (export.name(), export.ty()) {
            ("memory", ExternType::Memory(m)) => {
                memory_ok = m.minimum() <= manifest.memory_pages
                    && m.maximum().is_some_and(|max| max <= manifest.memory_pages)
            }
            ("alloc", ExternType::Func(f)) => {
                alloc_ok = f.params() == [ValType::I32] && f.results() == [ValType::I32]
            }
            ("run_json", ExternType::Func(f)) => {
                run_ok = f.params() == [ValType::I32, ValType::I32] && f.results() == [ValType::I64]
            }
            (_, ExternType::Table(t))
                if t.minimum() > 65_536 || t.maximum().is_none_or(|max| max > 65_536) =>
            {
                return Err(Error::Module);
            }
            _ => {}
        }
    }
    if !memory_ok || !alloc_ok || !run_ok {
        return Err(Error::Module);
    }
    let limits = StoreLimitsBuilder::new()
        .memory_size((manifest.memory_pages * 65_536) as usize)
        .table_elements(65_536)
        .instances(1)
        .tables(1)
        .memories(1)
        .build();
    let mut store = Store::new(&engine, State { limits });
    store.limiter(|state| &mut state.limits);
    store
        .set_fuel(manifest.fuel)
        .map_err(|_| Error::Execution)?;
    let instance = Linker::<State>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .map_err(|_| Error::Execution)?;
    let memory = instance.get_memory(&store, "memory").ok_or(Error::Module)?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&store, "alloc")
        .map_err(|_| Error::Module)?;
    let run = instance
        .get_typed_func::<(i32, i32), i64>(&store, "run_json")
        .map_err(|_| Error::Module)?;
    let payload = serde_json::to_vec(input).map_err(|_| Error::Internal)?;
    let len = i32::try_from(payload.len()).map_err(|_| Error::Protocol)?;
    let ptr = alloc.call(&mut store, len).map_err(|_| Error::Execution)?;
    if ptr < 0 {
        return Err(Error::Execution);
    }
    memory
        .write(&mut store, ptr as usize, &payload)
        .map_err(|_| Error::Execution)?;
    let packed = run
        .call(&mut store, (ptr, len))
        .map_err(|_| Error::Execution)? as u64;
    let out_ptr = (packed >> 32) as usize;
    let out_len = (packed & 0xffff_ffff) as usize;
    if out_len == 0 || out_len > MAX_RESPONSE {
        return Err(Error::Decision);
    }
    let mut result = vec![0; out_len];
    memory
        .read(&store, out_ptr, &mut result)
        .map_err(|_| Error::Execution)?;
    let decision: Decision = serde_json::from_slice(&result).map_err(|_| Error::Decision)?;
    input.validate_decision(&decision)?;
    Ok(decision)
}
