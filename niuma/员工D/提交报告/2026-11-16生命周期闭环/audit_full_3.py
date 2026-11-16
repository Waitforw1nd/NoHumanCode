"""Read completed full-3 evidence only. Never invokes Cargo or product binaries."""
from pathlib import Path
import hashlib
import json
import re
import subprocess
from datetime import datetime, timezone

REPORT = Path(__file__).resolve().parent
REPO = REPORT.parents[3]
INTEGRATION = REPO.parent / "nhc-lifecycle-20261116-integration"
ATTEMPT = INTEGRATION / ".local/lifecycle/integrated-full-3"
EXPECTED = {
    "head": "0fb205851748fdf25abdf35ca0911a0b24527fd4",
    "product_tree": "dec9a1e0fc0fbd0a0822dc49b41124015c4202fe",
    "rust_tree": "a4cee432f0a2be0d326519840881d8c7d1b12b26",
}
if not (ATTEMPT / "manifest.json").is_file():
    raise SystemExit("Pending final manifest; no evidence report written")
blobs = {p.name: p.read_bytes() for p in ATTEMPT.iterdir() if p.is_file()}
sha = lambda raw: hashlib.sha256(raw).hexdigest()
manifest = json.loads(blobs["manifest.json"])
targets = ["approval_gate", "approval_http", "cli", "host_configuration", "host_configuration_security", "network_authorization", "runtime", "session_turns", "turn_http"]
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
    "execution_length_consistent": (len(manifest["steps"]) == len(plan) if manifest["status"] == "passed"
        else len(manifest["steps"]) <= len(plan) and manifest["steps"][-1]["exit_code"] != 0),
    "options": manifest["options"] == dict(mode="full", test_targets=targets, filter="", examples=[], step_timeout_seconds=1800),
    "source_equal": before == after and blobs["source-before.json"] == blobs["source-after.json"],
    "clean": before["clean"] and not before["status"],
    "source_digest": sha((json.dumps(before["files"], ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode()) == before["source_sha256"],
    "artifact_set": {e["path"] for e in manifest["artifacts"]} <= set(blobs) - {"manifest.json"},
    "artifact_hashes": all(sha(blobs[e["path"]]) == e["sha256"] and len(blobs[e["path"]]) == e["bytes"] for e in manifest["artifacts"]),
    "git_trees": all(subprocess.check_output(["git", "-C", str(INTEGRATION), "rev-parse", EXPECTED["head"] + suffix]).decode().strip() == EXPECTED[key] for key, suffix in [("product_tree", ":NoManCode"), ("rust_tree", ":NoManCode/rust-app")]),
    "no_cleanup_overclaim": manifest["gate_retained"] and manifest["process_tree_cleanup"] == "not_proven_by_this_runner",
}
expected_artifacts = {"source-before.json", "source-after.json"}
for i, step in enumerate(manifest["steps"], 1):
    expected_artifacts.update(f"{i:02d}-{step['name']}" + suffix for suffix in (".log", ".started.json", ".direct-exit.json", ".result.json"))
checks["manifest_artifact_completeness"] = {e["path"] for e in manifest["artifacts"]} == expected_artifacts
checks["unique_artifact_paths"] = len(expected_artifacts) == len(manifest["artifacts"])
checks["success_exit_consistency"] = (manifest["status"] == "passed") == (manifest["runner_exit_code"] == 0)
checks["environment_limits"] = all(manifest["environment"].get(k) == v for k,v in {
    "CARGO_BUILD_JOBS":"2", "CARGO_INCREMENTAL":"0", "CARGO_NET_OFFLINE":"true",
    "CARGO_PROFILE_DEV_DEBUG":"0", "CARGO_PROFILE_TEST_DEBUG":"0", "RUST_TEST_THREADS":"1"}.items())
checks["output_paths_e_only"] = all(manifest["environment"][k].lower().startswith("e:") for k in ("CARGO_TARGET_DIR", "TMP", "TEMP"))
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
    declared_counts = {}
    for line_number, line in enumerate(log.decode("utf-8", errors="replace").splitlines(), 1):
        if re.search(r"Running (unittests|tests/|tests\\)|Doc-tests", line):
            section = line.strip()
        running = re.fullmatch(r"running (\d+) tests?", line)
        if running and section and section not in declared_counts:
            declared_counts[section] = int(running[1])
        match = re.match(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;", line)
        if match:
            summaries.append(dict(line=line_number, section=section, status=match[1], passed=int(match[2]), failed=int(match[3]), ignored=int(match[4]), measured=int(match[5]), filtered=int(match[6])))
    # A current_exe child may print its own test result in the parent section.
    # The final result before the next Cargo Running/Doc-tests header is the outer summary.
    sections = {}
    for summary in summaries:
        sections.setdefault(summary["section"], []).append(summary)
    outer = [rows[-1] for rows in sections.values()]
    nested = [row for rows in sections.values() for row in rows[:-1]]
    if step["exit_code"] == 0 and (name == "test" or name.startswith("target-")):
        checks[prefix + "_outer_declared_counts"] = (set(declared_counts) == set(sections)
            and all(declared_counts[row["section"]] == sum(row[k] for k in ("passed", "failed", "ignored", "measured")) for row in outer))
    totals = {key: sum(row[key] for row in outer) for key in ("passed", "failed", "ignored", "measured", "filtered")}
    web_pass = [line for line in log.decode("utf-8", errors="replace").splitlines() if line.startswith("PASS ")]
    if step["exit_code"] == 0 and (name == "test" or name.startswith("target-")):
        checks[prefix + "_test_summaries"] = bool(outer) and all(row["status"] == "ok" and row["failed"] == 0 for row in outer)
    if name == "test" and step["exit_code"] == 0:
        checks[prefix + "_web_six"] = len(web_pass) == 6 and len(set(web_pass)) == 6
    steps.append(dict(name=name, exit_code=step["exit_code"], terminal_unknown=step["terminal_unknown"], sha256=sha(log), bytes=len(log), rust_summaries=summaries, top_level_summaries=outer, nested_summaries_excluded=nested, top_level_totals=totals, declared_counts=declared_counts, web_pass=web_pass))
checks["final_time"] = previous <= datetime.fromisoformat(manifest["ended_utc"])
checks["manifest_stable_during_audit"] = (ATTEMPT / "manifest.json").read_bytes() == blobs["manifest.json"]
result = dict(checked_utc=datetime.now(timezone.utc).isoformat(), candidate=EXPECTED,
    scope="Fixed full-3 complete evidence; targeted repeats and nested child results excluded from full-suite totals; component25 separately source-bound",
    recorded_status=manifest["status"], runner_exit_code=manifest["runner_exit_code"],
    planned_count=len(plan), completed_count=len(manifest["steps"]), checks=checks,
    mismatches=[k for k, v in checks.items() if not v], steps=steps,
    files=[dict(path=name, sha256=sha(raw), bytes=len(raw)) for name, raw in sorted(blobs.items())])
result["full_suite"] = next((step for step in steps if step["name"] == "test"), None)
result["targeted_totals"] = {key: sum(step["top_level_totals"][key] for step in steps if step["name"].startswith("target-")) for key in ("passed", "failed", "ignored")}
result["manifest_sha256"] = sha(blobs["manifest.json"])
result["extra_files_not_in_manifest"] = sorted(set(blobs) - {e["path"] for e in manifest["artifacts"]} - {"manifest.json"})
with (REPORT / "full-3完整证据复核.json").open("x", encoding="utf-8", newline="\n") as stream:
    json.dump(result, stream, ensure_ascii=False, indent=2)
print(json.dumps({k: result[k] for k in ("recorded_status", "runner_exit_code", "planned_count", "completed_count", "targeted_totals", "mismatches")}))
if result["full_suite"]:
    print(json.dumps({"full_suite_totals": result["full_suite"]["top_level_totals"], "nested_summaries_excluded": len(result["full_suite"]["nested_summaries_excluded"]), "web_groups": len(result["full_suite"]["web_pass"])}))
raise SystemExit(0 if not result["mismatches"] else 1)
