use peachsh::wasm_worker_executor::execute;
use peachsh::wasm_worker_protocol::{
    self as p, Decision, Error, Input, Manifest, Outcome, Phase, Request, Response, ToolClass, Vote,
};
use sha2::{Digest, Sha256};
use std::io::{self, Read};

fn manifest(module: &[u8]) -> Manifest {
    Manifest {
        version: 1,
        request_id: "request-1".into(),
        nonce: "00112233445566778899aabbccddeeff".into(),
        binding_generation: 2,
        plugin_id: "agent-one".into(),
        plugin_version: "1.0.0".into(),
        digest: format!("{:x}", Sha256::digest(module)),
        fuel: 100_000,
        memory_pages: 1,
    }
}
fn before() -> Input {
    Input {
        round: 0,
        max_rounds: 3,
        decision: Phase::Before {
            tools: vec![ToolClass::Read, ToolClass::Write],
        },
    }
}
fn module_with_output(output: &str, extra: &str) -> Vec<u8> {
    let bytes = output
        .bytes()
        .map(|b| format!("\\{b:02x}"))
        .collect::<String>();
    wat::parse_str(format!(
        r#"(module
      (memory (export "memory") 1 1)
      (global $count (mut i32) (i32.const 0))
      (func $start global.get $count i32.const 1 i32.add global.set $count)
      (start $start)
      (data (i32.const 1024) "{bytes}")
      (func (export "alloc") (param i32) (result i32) i32.const 0)
      (func (export "run_json") (param i32 i32) (result i64)
        global.get $count i32.const 1 i32.ne if unreachable end
        i64.const {}) {extra})"#,
        (1024_u64 << 32) | output.len() as u64
    ))
    .unwrap()
}
#[test]
fn real_wasm_single_start_and_valid_phases() {
    let a = module_with_output(r#"{"action":"use_tools","indices":[1]}"#, "");
    assert_eq!(
        execute(&manifest(&a), &before(), &a).unwrap(),
        Decision::UseTools { indices: vec![1] }
    );
    let b = module_with_output(r#"{"action":"tool_votes","votes":["deny"]}"#, "");
    let input = Input {
        round: 1,
        max_rounds: 3,
        decision: Phase::AfterTools {
            tools: vec![ToolClass::Command],
        },
    };
    assert_eq!(
        execute(&manifest(&b), &input, &b).unwrap(),
        Decision::ToolVotes {
            votes: vec![Vote::Deny]
        }
    );
    let c = module_with_output(r#"{"action":"finish"}"#, "");
    let input = Input {
        round: 1,
        max_rounds: 3,
        decision: Phase::AfterNoTools {},
    };
    assert_eq!(
        execute(&manifest(&c), &input, &c).unwrap(),
        Decision::Finish {}
    );
}
fn check_wat(wat: &str) -> Result<Decision, Error> {
    let m = wat::parse_str(wat).unwrap();
    execute(&manifest(&m), &before(), &m)
}
#[test]
fn rejects_module_errors_and_budget() {
    let cases = [
        r#"(module (import "x" "y" (func)) (memory (export "memory") 1 1) (func (export "alloc") (param i32) (result i32) i32.const 0) (func (export "run_json") (param i32 i32) (result i64) i64.const 0))"#,
        r#"(module (memory (export "memory") 1 1) (func (export "alloc") (result i32) i32.const 0) (func (export "run_json") (param i32 i32) (result i64) i64.const 0))"#,
        r#"(module (memory (export "memory") 1) (func (export "alloc") (param i32) (result i32) i32.const 0) (func (export "run_json") (param i32 i32) (result i64) i64.const 0))"#,
        r#"(module (table (export "t") 65537 65537 funcref) (memory (export "memory") 1 1) (func (export "alloc") (param i32) (result i32) i32.const 0) (func (export "run_json") (param i32 i32) (result i64) i64.const 0))"#,
        r#"(module (memory (export "memory") 1 1) (func (export "alloc") (param i32) (result i32) i32.const -1) (func (export "run_json") (param i32 i32) (result i64) i64.const 0))"#,
        r#"(module (memory (export "memory") 1 1) (func (export "alloc") (param i32) (result i32) i32.const 0) (func (export "run_json") (param i32 i32) (result i64) (loop $l br $l) i64.const 0))"#,
    ];
    let expected = [
        Error::Module,
        Error::Module,
        Error::Module,
        Error::Module,
        Error::Execution,
        Error::Execution,
    ];
    for (case, expected) in cases.into_iter().zip(expected) {
        assert_eq!(check_wat(case), Err(expected));
    }
    let start_loop = r#"(module
        (memory (export "memory") 1 1)
        (func $start (loop $l br $l))
        (start $start)
        (func (export "alloc") (param i32) (result i32) i32.const 0)
        (func (export "run_json") (param i32 i32) (result i64) i64.const 0))"#;
    assert_eq!(check_wat(start_loop), Err(Error::Execution));
    let bad = module_with_output(r#"{"action":"use_tools","indices":[1,0]}"#, "");
    assert_eq!(
        execute(&manifest(&bad), &before(), &bad),
        Err(Error::Decision)
    );
}
struct GuardReader {
    data: Vec<u8>,
    pos: usize,
}
impl Read for GuardReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        assert!(self.pos < 4, "oversized frame payload was read");
        let count = buf.len().min(4 - self.pos);
        buf[..count].copy_from_slice(&self.data[self.pos..self.pos + count]);
        self.pos += count;
        Ok(count)
    }
}
#[test]
fn framing_rejects_lengths_truncation_tail_and_identity() {
    let mut guard = GuardReader {
        data: ((p::MAX_MANIFEST + 1) as u32).to_be_bytes().to_vec(),
        pos: 0,
    };
    assert!(p::read_request(&mut guard).is_err());
    let m = module_with_output(r#"{"action":"finish"}"#, "");
    let req = Request {
        manifest: manifest(&m),
        input: before(),
        module: m,
    };
    let mut bytes = Vec::new();
    p::write_request(&mut bytes, &req).unwrap();
    assert!(p::read_request(&mut &bytes[..bytes.len() - 1]).is_err());
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(p::read_request(&mut &extra[..]).is_err());
    assert!(p::read_request(&mut &bytes[..]).is_ok());
    let response = Response {
        manifest: req.manifest.clone(),
        outcome: Outcome::Ok {
            decision: Decision::UseTools { indices: vec![0] },
        },
    };
    let mut result = Vec::new();
    p::write_response(&mut result, &response).unwrap();
    let mut wrong = req.manifest.clone();
    wrong.binding_generation += 1;
    assert_eq!(
        p::read_response(&mut &result[..], &wrong, &req.input),
        Err(Error::Identity)
    );
    let mut out_of_phase = req.input.clone();
    out_of_phase.decision = Phase::AfterNoTools {};
    assert_eq!(
        p::read_response(&mut &result[..], &req.manifest, &out_of_phase),
        Err(Error::Decision)
    );
}

#[test]
fn decision_validation_and_strict_json() {
    let input = before();
    assert_eq!(
        input.validate_decision(&Decision::UseTools { indices: vec![2] }),
        Err(Error::Decision)
    );
    assert_eq!(
        input.validate_decision(&Decision::UseTools {
            indices: vec![1, 1]
        }),
        Err(Error::Decision)
    );
    let after = Input {
        round: 0,
        max_rounds: 1,
        decision: Phase::AfterTools {
            tools: vec![ToolClass::Read],
        },
    };
    assert_eq!(
        after.validate_decision(&Decision::ToolVotes { votes: vec![] }),
        Err(Error::Decision)
    );
    assert!(
        serde_json::from_str::<Input>(
            r#"{"round":0,"max_rounds":1,"decision":{"phase":"after_no_tools"},"messages":[]}"#
        )
        .is_err()
    );
    assert!(serde_json::from_str::<Decision>(r#"{"action":"finish","secret":"x"}"#).is_err());
    assert!(
        serde_json::from_str::<Input>(
            r#"{"round":0,"max_rounds":1,"decision":{"phase":"before","tools":["arbitrary"]}}"#
        )
        .is_err()
    );
    let bad = module_with_output(r#"{"action":"use_tools","indices":[999]}"#, "");
    assert_eq!(execute(&manifest(&bad), &input, &bad), Err(Error::Decision));
    let invalid = module_with_output("not-json", "");
    assert_eq!(
        execute(&manifest(&invalid), &input, &invalid),
        Err(Error::Decision)
    );
    let mut wrong_digest = manifest(&invalid);
    wrong_digest.digest = "0".repeat(64);
    assert_eq!(
        execute(&wrong_digest, &input, &invalid),
        Err(Error::Manifest)
    );
}

#[test]
fn protocol_rejects_wrong_version_invalid_utf8_and_response_tail() {
    let module = module_with_output(r#"{"action":"finish"}"#, "");
    let mut identity = manifest(&module);
    identity.version = 2;
    assert_eq!(identity.validate(), Err(Error::Manifest));
    let mut invalid_utf8 = vec![0, 0, 0, 1, 0xff];
    assert!(p::read_request(&mut &invalid_utf8[..]).is_err());
    invalid_utf8.clear();
    let identity = manifest(&module);
    let response = Response {
        manifest: identity.clone(),
        outcome: Outcome::Error {
            error: Error::Module,
        },
    };
    p::write_response(&mut invalid_utf8, &response).unwrap();
    invalid_utf8.push(0);
    assert!(p::read_response(&mut &invalid_utf8[..], &identity, &before()).is_err());
}

fn test_frame(output: &mut Vec<u8>, payload: &[u8]) {
    output.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    output.extend_from_slice(payload);
}

#[test]
fn unit_variants_reject_unknown_fields_at_framed_boundaries() {
    let module = module_with_output(r#"{"action":"finish"}"#, "");
    let identity = manifest(&module);
    let mut request = Vec::new();
    test_frame(&mut request, &serde_json::to_vec(&identity).unwrap());
    test_frame(
        &mut request,
        br#"{"round":0,"max_rounds":1,"decision":{"phase":"after_no_tools","extra":"forbidden"}}"#,
    );
    test_frame(&mut request, &module);
    assert_eq!(
        p::read_request(&mut &request[..]).err(),
        Some(Error::Protocol)
    );

    let input = Input {
        round: 0,
        max_rounds: 1,
        decision: Phase::AfterNoTools {},
    };
    let mut valid_request = Vec::new();
    p::write_request(
        &mut valid_request,
        &Request {
            manifest: identity.clone(),
            input: input.clone(),
            module,
        },
    )
    .unwrap();
    assert!(p::read_request(&mut &valid_request[..]).is_ok());
    for action in ["finish", "stop"] {
        let mut response = Vec::new();
        let body = serde_json::json!({
            "manifest": identity,
            "outcome": {"status": "ok", "decision": {"action": action, "extra": "forbidden"}}
        });
        test_frame(&mut response, &serde_json::to_vec(&body).unwrap());
        assert_eq!(
            p::read_response(&mut &response[..], &identity, &input),
            Err(Error::Protocol)
        );
    }
    for decision in [Decision::Finish {}, Decision::Stop {}] {
        let mut response = Vec::new();
        p::write_response(
            &mut response,
            &Response {
                manifest: identity.clone(),
                outcome: Outcome::Ok {
                    decision: decision.clone(),
                },
            },
        )
        .unwrap();
        assert_eq!(
            p::read_response(&mut &response[..], &identity, &input)
                .unwrap()
                .outcome,
            Outcome::Ok { decision }
        );
    }
}

fn catalog_manifest(id: &str, version: (u32, u32, u32)) -> String {
    serde_json::json!({
        "manifest_version": 1,
        "id": id,
        "display_name": "Wasm strategy",
        "version": {"major": version.0, "minor": version.1, "patch": version.2},
        "scope": "host", "runtime": "wasm", "lifecycle": "host_managed",
        "surfaces": ["cli"], "permissions": [], "config_schema": {},
        "provides": [{"kind":"service","name":"agent.strategy","version":{"major":1,"minor":0,"patch":0}}],
        "requires": []
    }).to_string()
}

#[test]
fn catalog_id_and_exact_version_survive_framed_worker_protocol() {
    let module = module_with_output(r#"{"action":"finish"}"#, "");
    let ids = [
        "a1.b-c_d".to_owned(),
        "a".to_owned(),
        "0".to_owned(),
        format!("a{}", "z".repeat(63)),
    ];
    for id in ids {
        let catalog =
            peachsh::plugin_catalog::parse_manifest(&catalog_manifest(&id, (1, 2, 3))).unwrap();
        let mut identity = manifest(&module);
        identity.plugin_id = catalog.id.clone();
        identity.plugin_version = catalog.version.to_string();
        let request = Request {
            manifest: identity.clone(),
            input: before(),
            module: module.clone(),
        };
        let mut frame = Vec::new();
        p::write_request(&mut frame, &request).unwrap();
        let actual = p::read_request(&mut &frame[..]).unwrap();
        assert_eq!(actual.manifest.plugin_id, catalog.id);
        assert_eq!(actual.manifest.plugin_version, catalog.version.to_string());
        assert_eq!(actual.manifest, identity);
    }
}

#[test]
fn worker_identity_rules_match_catalog_and_canonical_versions() {
    let module = module_with_output(r#"{"action":"finish"}"#, "");
    for id in [
        "A",
        "aB",
        ".first",
        "_first",
        "-first",
        "slash/name",
        "back\\name",
        "",
        &"a".repeat(65),
    ] {
        let catalog_valid =
            peachsh::plugin_catalog::parse_manifest(&catalog_manifest(id, (0, 0, 0))).is_ok();
        let mut identity = manifest(&module);
        identity.plugin_id = id.to_owned();
        assert!(!catalog_valid, "catalog unexpectedly accepted {id:?}");
        assert_eq!(
            identity.validate(),
            Err(Error::Manifest),
            "worker accepted {id:?}"
        );
    }
    let mut identity = manifest(&module);
    for version in ["0.0.0", "4294967295.4294967295.4294967295"] {
        identity.plugin_version = version.into();
        assert_eq!(identity.validate(), Ok(()), "rejected {version}");
    }
    for version in [
        "4294967296.0.0",
        "0.4294967296.0",
        "0.0.4294967296",
        "00.0.0",
        "0.01.0",
        "0.0.01",
        "1.2",
        "1.2.3.4",
        "1..3",
        ".1.2",
        "1.2.",
        "1.2.3-beta",
        "+1.2.3",
        "1.2.x",
    ] {
        identity.plugin_version = version.into();
        assert_eq!(
            identity.validate(),
            Err(Error::Manifest),
            "accepted {version}"
        );
    }
}
