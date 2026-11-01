//! Component tests for the trusted worker supervisor.
use super::wasm_worker_process::{TrustedWorker, WorkerService};
use super::wasm_worker_protocol::{
    self as protocol, Decision, Input, Manifest, Phase, ToolClass, Vote,
};
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};
use tokio_util::sync::CancellationToken;

fn worker() -> WorkerService {
    // The outer runner builds the real W1 binary last, copies it to a fixed
    // snapshot and supplies both path and hash. This entry exists only in tests.
    let path =
        PathBuf::from(std::env::var_os("NHC_W2_WORKER_SNAPSHOT").expect("fixed worker snapshot"));
    let expected = std::env::var("NHC_W2_WORKER_SHA256").expect("fixed worker digest");
    let digest: [u8; 32] = Sha256::digest(fs::read(&path).expect("read worker snapshot")).into();
    assert_eq!(
        format!("{:x}", Sha256::digest(fs::read(&path).unwrap())),
        expected
    );
    WorkerService::from_trusted(TrustedWorker::from_host_install(path, digest))
}

fn wasm() -> Vec<u8> {
    let before = r#"{"action":"use_tools","indices":[1]}"#;
    let after = r#"{"action":"tool_votes","votes":["deny","apply_host_policy"]}"#;
    let encode = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let before_bytes = encode(before);
    let after_bytes = encode(after);
    wat::parse_str(format!(
        r#"(module
          (memory (export "memory") 1 1)
          (global $started (mut i32) (i32.const 0))
          (func $start i32.const 7 global.set $started)
          (start $start)
          (data (i32.const 1024) "{before_bytes}")
          (data (i32.const 1200) "{after_bytes}")
          (func (export "alloc") (param i32) (result i32) i32.const 0)
          (func (export "run_json") (param $p i32) (param i32) (result i64)
            global.get $started i32.const 7 i32.ne if unreachable end
            local.get $p i32.const 9 i32.add i32.load8_u i32.const 48 i32.eq
            if (result i64) i64.const {} else i64.const {} end))"#,
        (1024u64 << 32) | before.len() as u64,
        (1200u64 << 32) | after.len() as u64
    ))
    .unwrap()
}

fn leb(mut value: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            return bytes;
        }
    }
}
fn near_limit(mut module: Vec<u8>) -> Vec<u8> {
    let target = protocol::MAX_MODULE - 64;
    let mut payload = target - module.len() - 6;
    while module.len() + 1 + leb(payload).len() + payload < target {
        payload += 1;
    }
    assert_eq!(module.len() + 1 + leb(payload).len() + payload, target);
    module.push(0); // WebAssembly custom section.
    module.extend(leb(payload));
    module.extend([1, b'x']);
    module.resize(target, 0);
    module
}

fn manifest() -> Manifest {
    Manifest {
        version: protocol::VERSION,
        request_id: "replaced-by-host".into(),
        nonce: "00000000000000000000000000000000".into(),
        binding_generation: 19,
        plugin_id: "w2-test".into(),
        plugin_version: "1.0.0".into(),
        digest: "0".repeat(64),
        fuel: 1_000_000,
        memory_pages: 1,
    }
}

#[tokio::test]
async fn w01_real_worker_before_after_and_near_8mib() {
    let stage_log = PathBuf::from(std::env::var_os("NHC_W2_STAGE_LOG").expect("attempt stage log"));
    assert!(
        !stage_log.exists(),
        "stage log must belong to this attempt only"
    );
    let service = worker();
    let before = Input {
        round: 0,
        max_rounds: 3,
        decision: Phase::Before {
            tools: vec![ToolClass::Read, ToolClass::Write],
        },
    };
    let module = near_limit(wasm());
    assert!(module.len() > 8 * 1024 * 1024 - 128);
    let first = service
        .call(manifest(), before, module, CancellationToken::new())
        .await
        .expect("real before policy");
    println!(
        "W01 before pid={} exit={} signaled={} job_active={} io_completed={} module_bytes={}",
        first.process_id,
        first.exit_code,
        first.process_signaled,
        first.job_active_processes,
        first.io_completed,
        protocol::MAX_MODULE - 64
    );
    assert_eq!(first.decision, Decision::UseTools { indices: vec![1] });
    assert!(first.process_id != 0 && first.process_signaled && first.io_completed);
    assert_eq!((first.exit_code, first.job_active_processes), (0, 0));

    let after = Input {
        round: 1,
        max_rounds: 3,
        decision: Phase::AfterTools {
            tools: vec![ToolClass::Command, ToolClass::Read],
        },
    };
    let module = wasm();
    let second = service
        .call(manifest(), after, module, CancellationToken::new())
        .await
        .expect("real after policy");
    println!(
        "W01 after pid={} exit={} signaled={} job_active={} io_completed={}",
        second.process_id,
        second.exit_code,
        second.process_signaled,
        second.job_active_processes,
        second.io_completed
    );
    assert_eq!(
        second.decision,
        Decision::ToolVotes {
            votes: vec![Vote::Deny, Vote::ApplyHostPolicy]
        }
    );
    assert!(second.process_id != 0 && second.process_signaled && second.io_completed);
    assert_eq!((second.exit_code, second.job_active_processes), (0, 0));
    let stages = fs::read_to_string(stage_log).expect("read supervisor stages");
    for pid in [first.process_id, second.process_id] {
        for phase in ["created_suspended", "resumed", "io_complete_reaped"] {
            assert!(
                stages.contains(&format!("phase={phase} pid=Some({pid})")),
                "missing {phase} for worker {pid}"
            );
        }
    }
}
