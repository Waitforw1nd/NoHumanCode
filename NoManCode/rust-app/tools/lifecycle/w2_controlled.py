"""Manager-only W2 matrix under the shared lifecycle gate; no independent lock."""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import uuid

from lifecycle_gates import (ENVIRONMENT, REPO, RUST_REL, ExecutionResult, digest, e_path,
                             execute, git_text, run_attempt, write_new)

TARGET = "x86_64-pc-windows-msvc"
PREFIX = "wasm_worker_process_tests::fault_matrix::"
CASES = (
    "wasm_worker_process_tests::w01_real_worker_before_after_and_near_8mib",
    *(PREFIX + name for name in (
        "w02_protocol_fault_matrix", "w03_pipe_fault_matrix", "w04_resource_limits",
        "w04_cpu_budget_boundaries", "w04_injected_zero_read_sensitivity",
        "w04_unknown_cleanup_and_slot_reuse", "w04_cancellation_and_acceptance_boundaries",
        "w04_injected_membership_failure_reaps", "w04_persistent_observation_errors_retain_io_and_slot",
        "w05_host_kill_boundaries", "w06_p01_live_job_observation")),
    *("wasm_worker_process::platform::w06_artifact_tests::" + name for name in (
        "pin_file_counterexamples", "pin_existing_writable_mapping", "process_image_identity_counterexample")),
    "wasm_worker_process::platform::w06_handle_tests::sentinel_explicit_inheritance_control",
    PREFIX + "w06_non_whitelisted_event", PREFIX + "w06_job_handle_not_inherited",
    "wasm_worker_process::platform::w06_limit_tests::active_limit_single_factor",
    "wasm_worker_process::platform::w06_limit_tests::breakaway_single_factor",
    "wasm_worker_process::platform::r04_tests::suspended_process_in_real_job_requires_process_fallback",
    *(PREFIX + name for name in (
        "w07_bound_same_domain_and_cross_domain", "w07_bound_future_drop_retains_original_domain",
        "w07_bound_supervisor_panic_disconnect_seals_domain",
        "w07_bound_supervisor_panic_after_future_drop_seals_domain")),
)
HELPERS = (
    PREFIX + "w05_host_helper",
    "wasm_worker_process::platform::w06_handle_tests::positive_helper",
    "wasm_worker_process::platform::w06_limit_tests::limit_helper",
)
ARTIFACTS = {
    "worker": ("peachsh-wasm-worker", ["bin"], "src/bin/peachsh-wasm-worker.rs", False),
    "probe": ("wasm_worker_process_probe", ["example"], "examples/wasm_worker_process_probe.rs", False),
    "lib-test": ("peachsh", ["lib"], "src/lib.rs", True),
}


def select_artifacts(raw, repo, target, keys):
    found = {key: set() for key in keys}
    finished = []
    for line in raw.splitlines():
        try:
            item = json.loads(line)
        except (ValueError, UnicodeDecodeError):
            continue
        if not isinstance(item, dict):
            continue
        if item.get("reason") == "build-finished":
            finished.append(item.get("success"))
        if item.get("reason") != "compiler-artifact" or not item.get("executable"):
            continue
        desc = item.get("target", {})
        for key in keys:
            name, kind, source, test = ARTIFACTS[key]
            if (desc.get("name") == name and desc.get("kind") == kind and
                    item.get("profile", {}).get("test") is test and desc.get("src_path") and
                    (repo / RUST_REL / desc["src_path"]).resolve() == (repo / RUST_REL / source).resolve()):
                path = (repo / RUST_REL / item["executable"]).resolve()
                if not path.is_relative_to(target.resolve()) or path.drive.upper() != "E:":
                    raise ValueError("artifact escaped selected target")
                found[key].add(path)
    if finished != [True] or any(len(paths) != 1 for paths in found.values()):
        raise ValueError("need one exact artifact per target and one successful build-finished")
    return {key: next(iter(paths)) for key, paths in found.items()}


def freeze(source, destination):
    raw = source.read_bytes()
    if not raw:
        raise ValueError("empty artifact")
    with destination.open("xb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    sha = digest(raw)
    if digest(source.read_bytes()) != sha or digest(destination.read_bytes()) != sha or destination.stat().st_nlink != 1:
        raise ValueError("artifact changed or copy is not independent")
    return {"source": str(source), "file": destination.name, "sha256": sha, "bytes": len(raw)}


def verify(path, record):
    if path.is_symlink() or path.stat().st_nlink != 1:
        raise ValueError("copy is linked")
    raw = path.read_bytes()
    if len(raw) != record["bytes"] or digest(raw) != record["sha256"]:
        raise ValueError("fixed copy changed")


def modes(repo):
    source = (repo / RUST_REL / "tests/support/wasm_worker_process/prepare-probes.ps1").read_text(encoding="utf-8-sig")
    match = re.search(r"\$modes\s*=\s*@\(([^)]*)\)", source)
    if not match:
        raise ValueError("probe mode contract missing")
    names = re.findall(r"'([a-z0-9-]+)'", match.group(1))
    remainder = re.sub(r"'([a-z0-9-]+)'|[\s,]", "", match.group(1))
    if remainder or not names or len(set(names)) != len(names):
        raise ValueError("invalid probe mode contract")
    return names


def exact_one(raw):
    summaries = re.findall(rb"(?m)^test result: ok\. (\d+) passed; (\d+) failed;[^\r\n]*", raw)
    return summaries == [(b"1", b"0")] and b"not established" not in raw.lower()


def listing_matches(raw):
    lines = raw.decode("utf-8", errors="replace").splitlines()
    names = {line[:-6] for line in lines if line.endswith(": test")}
    discovered = {name for name in names if name.startswith((
        "wasm_worker_process_tests::", "wasm_worker_process::platform::w06_",
        "wasm_worker_process::platform::r04_tests::"))}
    return discovered == set(CASES).union(HELPERS)


def make_plan(selected, timeout, fixed):
    steps = [{"name": "build-bins", "stage": "bins"}, {"name": "build-lib", "stage": "lib"},
             {"name": "listing", "stage": "listing"}]
    steps += [{"name": f"case-{index:02d}", "stage": "case", "case": name}
              for index, name in enumerate(selected, 1)]
    for step in steps:
        if step["stage"] in ("bins", "lib"):
            step["argv"] = ["pwsh", "-NoProfile", "-File", "tools/lifecycle/invoke-w2-build.ps1", "-Stage", step["stage"]]
        elif step["stage"] == "listing":
            step["argv"] = [str(fixed / "lib-test.exe"), "--list", "--ignored", "--format", "terse"]
        else:
            step["argv"] = [str(fixed / "lib-test.exe"), "--exact", step["case"], "--ignored", "--nocapture", "--test-threads=1"]
        step["timeout_seconds"] = timeout
    return steps


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("head", "product-tree", "rust-tree", "attempt"):
        parser.add_argument("--" + name, required=True)
    choice = parser.add_mutually_exclusive_group(required=True)
    choice.add_argument("--case", action="append", choices=CASES)
    choice.add_argument("--all", action="store_true")
    parser.add_argument("--evidence-root", default=".local/lifecycle-w2")
    parser.add_argument("--target-dir", default=".local/lifecycle-w2-target")
    parser.add_argument("--step-timeout-seconds", type=int, default=1800)
    args = parser.parse_args()
    expected = {k: getattr(args, k) for k in ("head", "product_tree", "rust_tree")}
    if any(not re.fullmatch(r"[0-9a-f]{40}", value) for value in expected.values()):
        parser.error("full lower-case Git IDs required")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", args.attempt) or not 1 <= args.step_timeout_seconds <= 7200:
        parser.error("invalid attempt or timeout")
    selected = list(CASES if args.all else args.case)
    if len(selected) != len(set(selected)):
        parser.error("duplicate case")
    repo = e_path(REPO, ".")
    out = e_path(repo, args.evidence_root) / args.attempt
    target = e_path(repo, args.target_dir)
    if out.is_relative_to(repo / "NoManCode") or target.is_relative_to(repo / "NoManCode"):
        raise ValueError("outputs must be outside product")
    if out == target or out.is_relative_to(target) or target.is_relative_to(out):
        raise ValueError("target and attempt must not overlap")
    common = e_path(repo, git_text(repo, "rev-parse", "--git-common-dir"))
    env = dict(os.environ, **ENVIRONMENT, CARGO_TARGET_DIR=str(target), TMP=str(out / "tmp"), TEMP=str(out / "tmp"))
    for key in list(env):
        if key.upper() in ("NHC_PACKAGED_WORKER_SHA256", "NHC_PACKAGED_WORKER_PROTOCOL"):
            del env[key]
    fixed = out / "fixed"
    steps = make_plan(selected, args.step_timeout_seconds, fixed)
    records = {}

    def invoke(argv, cwd, environment, timeout, log):
        step = steps[len(records.get("_completed", []))]
        target.mkdir(parents=True, exist_ok=True)
        (out / "tmp").mkdir(exist_ok=True)
        fixed.mkdir(exist_ok=True)
        stage = step["stage"]
        if stage in ("bins", "lib"):
            result = execute(argv, cwd, environment, timeout, log)
            if result.terminal_unknown or result.exit_code:
                return result
            selected_files = select_artifacts(log.read_bytes(), repo, target,
                                               ("worker", "probe") if stage == "bins" else ("lib-test",))
            for key, source in selected_files.items():
                records[key] = freeze(source, fixed / (key + ".exe"))
            write_new(out / (stage + "-fixed.json"), {k: records[k] for k in selected_files})
        else:
            for key in ARTIFACTS:
                verify(fixed / (key + ".exe"), records[key])
            child_env = environment.copy()
            if stage == "listing":
                result = execute(argv, cwd, child_env, timeout, log)
                if not result.terminal_unknown and result.exit_code == 0:
                    if not listing_matches(log.read_bytes()):
                        return ExecutionResult(1, False, "ignored W2 listing differs from complete mapping")
            else:
                site = out / "cases" / step["name"]
                site.mkdir(parents=True, exist_ok=False)
                temp = site / "tmp"
                temp.mkdir()
                worker = site / "worker.exe"
                freeze(fixed / "worker.exe", worker)
                probe_dir = site / "probes"
                probe_dir.mkdir()
                copies = []
                for name in modes(repo):
                    path = probe_dir / (name + ".exe")
                    freeze(fixed / "probe.exe", path)
                    copies.append(path)
                for name in ("resumed", "pending"):
                    path = site / "w05" / name / "no-read.exe"
                    path.parent.mkdir(parents=True)
                    freeze(fixed / "probe.exe", path)
                    copies.append(path)
                (site / "w06").mkdir()
                stage_log = site / "host.stage"
                child_env.update(TMP=str(temp), TEMP=str(temp), NHC_W2_RUN_NONCE=uuid.uuid4().hex,
                                 NHC_W2_WORKER_SNAPSHOT=str(worker), NHC_W2_WORKER_SHA256=records["worker"]["sha256"],
                                 NHC_W2_FAULT_SHA256=records["probe"]["sha256"], NHC_W2_STAGE_LOG=str(stage_log),
                                 NHC_W2_FAULT_DIR=str(probe_dir), NHC_W05_ROOT=str(site / "w05"), NHC_W06_ROOT=str(site / "w06"))
                write_new(site / "case.json", {"test": step["case"], "worker": records["worker"], "probe": records["probe"]})
                stage_paths = [stage_log, *(site / "w05" / (name + "-host.stage")
                                            for name in ("before", "suspended", "resumed", "pending"))]
                result = execute(argv, cwd, child_env, timeout, log,
                                 unknown_files=tuple((path, b"phase=created_process_handle_unknown ") for path in stage_paths))
                if not result.terminal_unknown and result.exit_code == 0:
                    if not exact_one(log.read_bytes()):
                        return ExecutionResult(1, False, "case did not prove exactly one passing test")
                    verify(worker, records["worker"])
                    for path in copies:
                        verify(path, records["probe"])
            if result.terminal_unknown or result.exit_code:
                return result
            for key in ARTIFACTS:
                verify(fixed / (key + ".exe"), records[key])
        records.setdefault("_completed", []).append(stage)
        return result

    code, report = run_attempt(repo, out, common / "nhc-heavy-window.lock", expected, steps, env,
                               {"kind": "w2-controlled", "selected": selected, "mapped_count": len(CASES)}, invoke=invoke)
    print(f"status={report['status']} evidence={out} gate_retained=true", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
