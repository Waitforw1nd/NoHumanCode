"""Read-only evidence audit; never starts Cargo, a fixture, or a product process."""
import hashlib
import json
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    return json.loads(path.read_text(encoding="utf-8-sig"))


root = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
matrix = root / ".local/lifecycle/component-final-matrix-1"
build = root / ".local/lifecycle/component-final-build-1"
published = root / "niuma/项目经理/审查记录/2026-11-16生命周期闭环/验证/component-final-matrix-1"
checks = []
records = []


def check(name, condition):
    checks.append({"name": name, "passed": bool(condition)})


def record(base, item, group):
    path = base / item["path"]
    actual = {"bytes": path.stat().st_size, "sha256": sha(path)} if path.is_file() else None
    good = actual is not None and actual["bytes"] == item["bytes"] and actual["sha256"] == item["sha256"].lower()
    records.append({"group": group, "path": item["path"], "expected": item, "actual": actual, "passed": good})
    check(group + ":" + item["path"], good)


manifest = read(matrix / "evidence-manifest.json")
for item in manifest:
    record(matrix, item, "matrix-original")
check("matrix-unique-paths", len({x["path"] for x in manifest}) == len(manifest))
publication = read(published / "publication.json")
for item in publication["files"]:
    record(published, item, "publication")
    check("publication-byte-copy:" + item["path"], (published / item["path"]).read_bytes() == (matrix / item["path"]).read_bytes())
check("publication-keeps-original-manifest", (published / "evidence-manifest.json").read_bytes() == (matrix / "evidence-manifest.json").read_bytes())
check("publication-manifest-count", publication["all_original_manifest_hashes_verified"] == len(manifest))
check("matrix-no-incident", not (matrix / "incident.json").exists())
for name in ("source", "source-files"):
    check(name + "-before-after-bytes", (matrix / f"{name}-before.json").read_bytes() == (matrix / f"{name}-after.json").read_bytes())

source = read(matrix / "source-before.json")
check("matrix-clean", source["status"] == [] and source["index"] == [])
for field, spec in [("head", source["head"]), ("product", source["head"] + ":NoManCode"), ("rust", source["head"] + ":NoManCode/rust-app")]:
    actual = subprocess.check_output(["git", "rev-parse", spec], cwd=root, text=True).strip()
    check("candidate-git-" + field, actual == source[field])
check("publication-candidate", publication["candidate"] == source)

inputs = read(matrix / "inputs.json")
selection = read(root / ".local/lifecycle/component-final-build-1-selection.json")
bins = []
for key, filename in [("test", "lib-test.exe"), ("probe", "probe.exe")]:
    fixed = matrix / "fixed" / filename
    selected = Path(selection[key]["path"])
    actual = sha(fixed)
    check(key + "-frozen-hash", actual == inputs[key + "_sha256"] == selection[key]["sha256"])
    check(key + "-selected-path", str(selected) == inputs[key])
    bins.append({"kind": key, "frozen_relative_path": str(fixed.relative_to(root)), "sha256": actual,
                 "selected_current_sha256": sha(selected) if selected.is_file() else None})

bm = read(build / "manifest.json")
for item in bm["artifacts"]:
    record(build, item, "build-original")
check("build-passed", bm["status"] == "passed" and bm["runner_exit_code"] == 0 and bm["inputs_verified"] is True)
check("build-matrix-candidate", bm["candidate"] == selection["candidate"] == {"head": source["head"], "product_tree": source["product"], "rust_tree": source["rust"]})
check("build-before-after-bytes", (build / "source-before.json").read_bytes() == (build / "source-after.json").read_bytes())
build_source = read(build / "source-before.json")
check("build-clean", build_source["clean"] is True and not build_source["status"])
bf = {x["path"]: x for x in build_source["files"]}
mf = {x["path"]: x for x in read(matrix / "source-files-before.json")}
check("build-matrix-source-byte-equivalence", all(bf.get(k) == v for k, v in mf.items()))
expected_commands = [
    ["cargo", "test", "--offline", "--locked", "--lib", "--no-run", "--message-format=json"],
    ["cargo", "build", "--offline", "--locked", "--example", "process_lifetime_probe", "--message-format=json"],
]
check("build-plan-two-exact-commands", [p["command"] for p in bm["plan"]] == expected_commands and len(bm["steps"]) == 2)
for i, step in enumerate(bm["steps"]):
    prefix = f"{i+1:02d}-{step['name']}"
    check(prefix + "-plan", all(step[k] == bm["plan"][i][k] for k in ("name", "argv", "command", "timeout_seconds")))
    check(prefix + "-known-zero", step["exit_code"] == 0 and step["terminal_unknown"] is False and step["unknown_reason"] is None)
    check(prefix + "-log", sha(build / step["log"]) == step["log_sha256"] and (build / step["log"]).stat().st_size == step["log_bytes"])
    direct = read(build / (prefix + ".direct-exit.json"))
    result = read(build / (prefix + ".result.json"))
    check(prefix + "-direct-result-zero", direct["exit_code"] == result["exit_code"] == 0 and not direct["terminal_unknown"] and not result["terminal_unknown"])
    messages = []
    for line in (build / step["log"]).read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            messages.append(json.loads(line))
        except ValueError:
            pass
    check(prefix + "-cargo-finished", any(m.get("reason") == "build-finished" and m.get("success") is True for m in messages))
    key, name, kind = ("test", "peachsh", "lib") if i == 0 else ("probe", "process_lifetime_probe", "example")
    paths = {m["executable"] for m in messages if m.get("reason") == "compiler-artifact" and m.get("target", {}).get("name") == name and kind in m.get("target", {}).get("kind", []) and m.get("executable") and (i != 0 or m.get("profile", {}).get("test"))}
    check(prefix + "-cargo-artifact-selection", paths == {selection[key]["path"]})

env = bm["environment"]
check("build-limits", all(env[k] == v for k, v in {"CARGO_BUILD_JOBS": "2", "CARGO_INCREMENTAL": "0", "CARGO_NET_OFFLINE": "true", "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0", "RUST_TEST_THREADS": "1"}.items()))
check("build-e-only-output", all(env[k].lower().startswith("e:") for k in ["TMP", "TEMP", "CARGO_TARGET_DIR"]))
check("explicit-msvc-sdk-roots-recorded", bool(env["VCToolsInstallDir"]) and bool(env["WindowsSdkDir"]))
helper = root / ".local/manager-fixed-invoke.ps1"
check("build-helper-hash", sha(helper) == bm["options"]["helper_sha256"] == selection["helper_sha256"])
launcher = "NoManCode/rust-app/tests/support/wasm_worker_process/DirectProcess.cs"
check("launcher-input-source-hash", inputs["launcher_sha256"].lower() == mf[launcher]["sha256"])
runner = root / "NoManCode/rust-app/tests/support/process_lifetime/run-fixed.ps1"
check("runner-current-recorded-hash", sha(runner) == mf[str(runner.relative_to(root)).replace('\\', '/')]["sha256"])
plan_text = runner.read_text(encoding="utf-8").split("$cases = @(", 1)[1].split("\n    )", 1)[0]
plan = re.findall(r"'([^']+)'", plan_text)
cases = []
for number in range(1, 26):
    site = matrix / f"case-{number:02d}"
    case = read(site / "case.json")
    stdout = (site / "stdout").read_text(encoding="utf-8")
    stderr = (site / "stderr").read_text(encoding="utf-8")
    check(f"case-{number:02d}-passed", case["state"] == "passed" and case["exit_code"] == 0)
    check(f"case-{number:02d}-identity", case["pid"] > 0 and case["creation_filetime"] > 0)
    check(f"case-{number:02d}-exact-test", f"test {case['case']} ... ok" in stdout and len(re.findall(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", stdout)) == 1)
    check(f"case-{number:02d}-no-unknown", "NHC_TERMINAL_UNKNOWN" not in stdout + stderr)
    cases.append(case)
check("matrix-complete-plan", len(plan) == 25 and [c["case"] for c in cases] == plan and len(set(plan)) == 25)
complete = read(matrix / "complete.json")
check("matrix-completion", complete["state"] == "complete" and complete["cases"] == publication["cases"] == publication["passed"] == 25)

bridge_paths = [k for k in mf if any(x in k for x in ("process_lifetime", "DirectProcess.cs", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml")) or k in ("NoManCode/rust-app/build.ps1", "NoManCode/rust-app/src/lib.rs")]
bridge = []
for path in bridge_paths:
    current = sha(root / path)
    bridge.append({"path": path, "matrix_sha256": mf[path]["sha256"], "current_sha256": current, "equal": current == mf[path]["sha256"]})
current_head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
report = {"reviewer": "D read-only; no Cargo/process execution", "checked_utc": datetime.now(timezone.utc).isoformat(),
          "candidate": source, "matrix_manifest_sha256": sha(matrix / "evidence-manifest.json"),
          "matrix_records": len(manifest), "publication_records": len(publication["files"]), "build_records": len(bm["artifacts"]),
          "source_file_count": len(mf), "binaries": bins, "cases": cases,
          "build_environment": env, "checks": checks, "records": records,
          "passed_checks": sum(c["passed"] for c in checks), "failed_checks": [c for c in checks if not c["passed"]],
          "bridge_observed_head": current_head, "bridge_files": bridge,
          "bridge_all_equal": all(x["equal"] for x in bridge),
          "limits": ["Final suite tree not yet supplied; re-evaluate bridge files at that immutable HEAD.",
                     "Matrix covers listed component cases, not final Host/HTTP acceptance.",
                     "No PID snapshot is used as process-tree cleanup proof; common gate closure is manager-owned.",
                     "Frozen original binaries are authoritative; target output paths can be rebuilt later.",
                     "Historical unknown incidents remain unchanged."]}
with out.open("x", encoding="utf-8") as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write("\n")
print(json.dumps({k: report[k] for k in ["matrix_records", "publication_records", "build_records", "source_file_count", "passed_checks", "failed_checks", "bridge_observed_head", "bridge_all_equal"]}, ensure_ascii=False))
sys.exit(0 if not report["failed_checks"] else 1)
