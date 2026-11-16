"""Read immutable snapshots and Git blobs only; no product execution."""
import hashlib
import json
import subprocess
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()


def read(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def git(*args):
    return subprocess.check_output(["git", *args], cwd=root)


matrix = root / ".local/lifecycle/component-final-matrix-1"
full = root / ".local/lifecycle/integrated-full-3/source-before.json"
m = {x["path"]: x for x in read(matrix / "source-files-before.json")}
f = read(full)
ff = {x["path"]: x for x in f["files"]}
paths = [k for k in m if any(x in k for x in ("process_lifetime", "DirectProcess.cs", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml")) or k in ("NoManCode/rust-app/build.ps1", "NoManCode/rust-app/src/lib.rs")]
bridge = [{"path": p, "matrix": m[p], "full3": ff.get(p), "current_sha256": sha(root / p),
           "equal": m[p] == ff.get(p) and sha(root / p) == m[p]["sha256"]} for p in paths]
a = "14b65ae3b4e808e63c8de0799455dd24fc7947fd"
files = ["NoManCode/rust-app/tests/host_configuration.rs", "NoManCode/rust-app/tests/host_configuration_security.rs"]
blobs = [{"path": p, "a_blob": git("rev-parse", a + ":" + p).decode().strip(),
          "full3_blob": git("rev-parse", f["head"] + ":" + p).decode().strip()} for p in files]
old = git("show", a + "^:" + files[1]).decode()
new = git("show", a + ":" + files[1]).decode()


def scanner(s):
    return s.split("fn scan_persisted(", 1)[1].split("\nstruct Host", 1)[0]


incident = root / "niuma/项目经理/审查记录/2026-11-16生命周期闭环/验证/incident-cli-host-1"
closure = read(incident / "closure.json")
incident_records = []
for item in closure["files"]:
    path = incident / item["path"]
    incident_records.append({"path": item["path"], "bytes": path.stat().st_size, "sha256": sha(path),
                             "equal": path.stat().st_size == item["bytes"] and sha(path) == item["sha256"]})
action = read(incident / "action-terminal.json")
terminal = read(incident / "terminal.json")
report = {"mode": "read-only; no Cargo or real process tests", "full3_source_before_sha256": sha(full),
          "full3_candidate": {k: f[k] for k in ("head", "product_tree", "rust_tree", "clean", "status")},
          "current_head": git("rev-parse", "HEAD").decode().strip(), "bridge": bridge,
          "bridge_all_equal": all(r["equal"] for r in bridge), "a_commit": a, "a_full3_blobs": blobs,
          "a_files_equal_full3": all(r["a_blob"] == r["full3_blob"] for r in blobs),
          "scanner_body_byte_unchanged": scanner(old) == scanner(new),
          "incident_closure_sha256": sha(incident / "closure.json"), "incident_artifacts": incident_records,
          "incident_identity_equal": action["identity"] == terminal["identity"],
          "incident_capture_supervisor_exit": closure["capture_supervisor_exit"],
          "incident_action_supervisor_exit": closure["action_exit"],
          "incident_target_exit_codes": [action["exit_code"], terminal["exit_code"]],
          "incident_handles_signalled": [action["signalled"], terminal["original_handle_signalled"]],
          "incident_identity_binding": closure["identity_binding"], "incident_scope": closure["scope"],
          "scope": "Component matrix source-equivalence bridge only; full3 run is not yet accepted."}
report["passed"] = (report["bridge_all_equal"] and report["a_files_equal_full3"] and report["scanner_body_byte_unchanged"]
                    and all(r["equal"] for r in incident_records) and report["incident_identity_equal"])
with out.open("x", encoding="utf-8", newline="\n") as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write("\n")
print(json.dumps({k: report[k] for k in ("full3_candidate", "bridge_all_equal", "a_files_equal_full3", "scanner_body_byte_unchanged", "incident_target_exit_codes", "passed")}, ensure_ascii=False))
sys.exit(0 if report["passed"] else 1)
