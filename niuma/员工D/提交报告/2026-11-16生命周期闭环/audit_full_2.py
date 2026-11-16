"""Read completed full-2 evidence only. Never invokes Cargo or product binaries."""
from pathlib import Path
import hashlib
import json
import re
import subprocess
from datetime import datetime, timezone

REPORT = Path(__file__).resolve().parent
REPO = REPORT.parents[3]
INTEGRATION = REPO.parent / "nhc-lifecycle-20261116-integration"
ATTEMPT = INTEGRATION / ".local/lifecycle/integrated-full-2"
EXPECTED = {
    "head": "4b00c481486aa17dd0fc7e0056c334cc64f6c092",
    "product_tree": "ff49de8748250ece88324c20e1b63f8dc996729b",
    "rust_tree": "a092d0e2923d9224850a6dc5f06167d87fef9e47",
}
if not (ATTEMPT / "manifest.json").is_file():
    raise SystemExit("Pending final manifest; no evidence report written")
blobs = {p.name: p.read_bytes() for p in ATTEMPT.iterdir() if p.is_file()}
sha = lambda raw: hashlib.sha256(raw).hexdigest()
manifest = json.loads(blobs["manifest.json"])
targets = ["execution_authority_lifecycle", "lifecycle_compat", "git_diff"]
commands = [
    ("check", ["cargo", "check", "--workspace", "--all-targets", "--offline", "--locked"]),
    ("debug-bins", ["cargo", "build", "--bins", "--offline", "--locked"]),
    *[("target-" + target, ["cargo", "test", "--offline", "--locked", "--test", target, "--", "--test-threads=1"]) for target in targets],
    ("test", ["pwsh", "-NoProfile", "-File", "build.ps1", "-Action", "test"]),
    ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
    ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--offline", "--locked", "--", "-D", "warnings"]),
    ("wasm-check", ["cargo", "check", "-p", "peachsh-ui", "--target", "wasm32-unknown-unknown", "--offline", "--locked"]),
]
plan = []
for name, command in commands:
    argv = ["pwsh", "-NoProfile", "-File", "tools/lifecycle/invoke-step.ps1", "-Stage",
            "targeted" if name.startswith("target-") else name]
    if name.startswith("target-"):
        argv += ["-TestTarget", name.removeprefix("target-")]
    plan.append(dict(name=name, command=command, argv=argv, timeout_seconds=1800))
before = json.loads(blobs["source-before.json"])
after = json.loads(blobs["source-after.json"])
checks = {
    "candidate": manifest["candidate"] == EXPECTED and all(before[k] == v for k, v in EXPECTED.items()),
    "complete_plan": manifest["plan"] == plan,
    "execution_length_consistent": (len(manifest["steps"]) == 9 if manifest["status"] == "passed"
        else len(manifest["steps"]) < 9 and manifest["steps"][-1]["exit_code"] != 0),
    "options": manifest["options"] == dict(mode="full", test_targets=targets, filter="", examples=[], step_timeout_seconds=1800),
    "source_equal": before == after and blobs["source-before.json"] == blobs["source-after.json"],
    "clean": before["clean"] and not before["status"],
    "source_digest": sha((json.dumps(before["files"], ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()) == before["source_sha256"],
    "artifact_set": {e["path"] for e in manifest["artifacts"]} == set(blobs) - {"manifest.json"},
    "artifact_hashes": all(sha(blobs[e["path"]]) == e["sha256"] and len(blobs[e["path"]]) == e["bytes"] for e in manifest["artifacts"]),
    "git_trees": all(subprocess.check_output(["git", "-C", str(INTEGRATION), "rev-parse", EXPECTED["head"] + suffix]).decode().strip() == EXPECTED[key] for key, suffix in [("product_tree", ":NoManCode"), ("rust_tree", ":NoManCode/rust-app")]),
    "no_cleanup_overclaim": manifest["gate_retained"] and manifest["process_tree_cleanup"] == "not_proven_by_this_runner",
}
steps = []
previous = datetime.fromisoformat(manifest["started_utc"])
for index, step in enumerate(manifest["steps"]):
    name = step["name"]
    prefix = f"{index + 1:02d}-{name}"
    log = blobs[prefix + ".log"]
    direct = json.loads(blobs[prefix + ".direct-exit.json"])
    checks[prefix + "_records"] = (json.loads(blobs[prefix + ".result.json"]) == step
        and json.loads(blobs[prefix + ".started.json"]) == dict(plan[index], started_utc=step["started_utc"], exit_code=None)
        and all(step[k] == v for k, v in plan[index].items()))
    checks[prefix + "_exit"] = direct["exit_code"] == step["exit_code"] and direct["terminal_unknown"] == step["terminal_unknown"]
    checks[prefix + "_log"] = sha(log) == step["log_sha256"] and len(log) == step["log_bytes"]
    begin, end = (datetime.fromisoformat(step[k]) for k in ("started_utc", "ended_utc"))
    checks[prefix + "_serial"] = previous <= begin <= end
    previous = end
    summaries = []
    section = ""
    for line_number, line in enumerate(log.decode("utf-8", errors="replace").splitlines(), 1):
        if re.search(r"Running (unittests|tests/|tests\\)|Doc-tests", line):
            section = line.strip()
        match = re.match(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;", line)
        if match:
            summaries.append(dict(line=line_number, section=section, status=match[1], passed=int(match[2]), failed=int(match[3]), ignored=int(match[4]), measured=int(match[5]), filtered=int(match[6])))
    steps.append(dict(name=name, exit_code=step["exit_code"], terminal_unknown=step["terminal_unknown"], sha256=sha(log), bytes=len(log), rust_summaries=summaries))
checks["final_time"] = previous <= datetime.fromisoformat(manifest["ended_utc"])
result = dict(checked_utc=datetime.now(timezone.utc).isoformat(), candidate=EXPECTED,
    scope="Stage validation only; B5a9e57a5 pending, not the final tested tree or 25-case matrix",
    recorded_status=manifest["status"], runner_exit_code=manifest["runner_exit_code"],
    planned_count=len(plan), completed_count=len(manifest["steps"]), checks=checks,
    mismatches=[k for k, v in checks.items() if not v], steps=steps,
    files=[dict(path=name, sha256=sha(raw), bytes=len(raw)) for name, raw in sorted(blobs.items())])
with (REPORT / "full-2阶段证据复核.json").open("x", encoding="utf-8") as stream:
    json.dump(result, stream, ensure_ascii=False, indent=2)
print(json.dumps({k: result[k] for k in ("recorded_status", "runner_exit_code", "mismatches")}))
