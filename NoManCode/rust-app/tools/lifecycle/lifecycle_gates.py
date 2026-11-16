"""Serial, fail-closed gate evidence. Standard library only; no shell evaluation.

Derived from the 2026-11-16 audit run-gates.ps1 and publish_evidence.py.
A command exit is not proof that its descendants or external effects have ended.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid

RUST_REL = "NoManCode/rust-app"
REPO = Path(__file__).resolve().parents[4]
INPUTS = ["NoManCode", ".cargo", ".gitattributes", "Cargo.toml", "Cargo.lock",
          "rust-toolchain", "rust-toolchain.toml"]
ENVIRONMENT = {
    "CARGO_NET_OFFLINE": "true", "CARGO_BUILD_JOBS": "2",
    "CARGO_PROFILE_DEV_DEBUG": "0", "CARGO_PROFILE_TEST_DEBUG": "0",
    "CARGO_INCREMENTAL": "0", "RUST_TEST_THREADS": "1",
    "RUST_TEST_NOCAPTURE": "1",
    "PYTHONDONTWRITEBYTECODE": "1", "RUSTUP_AUTO_INSTALL": "0",
}
NO_WINDOW = getattr(subprocess, "CREATE_NO_WINDOW", 0)


def utc():
    return datetime.now(timezone.utc).isoformat()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def environment_value(environment, key):
    # dict(os.environ) on Windows normalizes keys to uppercase. This lookup is
    # for provenance only; do not rewrite the actual child environment.
    return next((value for name, value in environment.items() if name.casefold() == key.casefold()), None)


def encode(value):
    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode("utf-8")


def write_new(path, value):
    # No replace/overwrite, including a failed or interrupted attempt.
    with path.open("xb") as stream:
        stream.write(encode(value))
        stream.flush()
        os.fsync(stream.fileno())


def git(repo, *args):
    return subprocess.check_output(
        ["git", "-C", str(repo), *args], stderr=subprocess.PIPE,
        creationflags=NO_WINDOW,
    )


def git_text(repo, *args):
    return git(repo, *args).decode("utf-8").strip()


def e_path(repo, value):
    path = (repo / value).resolve()
    if path.drive.upper() != "E:":
        raise ValueError(f"All active paths must resolve to E: {path}")
    return path


def snapshot(repo):
    status = git(repo, "status", "--porcelain=v1", "-z", "--untracked-files=all", "--", *INPUTS)
    # Ignored local configuration must not invisibly override committed Cargo input.
    for base in (repo, repo / "NoManCode", repo / RUST_REL):
        for relative in (".cargo/config", ".cargo/config.toml", "rust-toolchain", "rust-toolchain.toml"):
            path = base / relative
            if path.exists() and not git(repo, "ls-files", "--", path.relative_to(repo).as_posix()).strip():
                raise ValueError(f"Untracked build configuration: {path}")
    names = git(repo, "ls-files", "-z", "--", *INPUTS).decode("utf-8").split("\0")
    files = []
    for name in sorted(filter(None, names)):
        path = repo / name
        if path.is_symlink() or not path.resolve().is_relative_to(repo.resolve()):
            raise ValueError(f"Build input escapes repository: {name}")
        if path.is_file():
            raw = path.read_bytes()
            files.append({"path": name, "bytes": len(raw), "sha256": digest(raw)})
        else:
            files.append({"path": name, "missing": True})
    return {
        "head": git_text(repo, "rev-parse", "HEAD"),
        "product_tree": git_text(repo, "rev-parse", "HEAD:NoManCode"),
        "rust_tree": git_text(repo, "rev-parse", "HEAD:" + RUST_REL),
        "clean": not bool(status), "status": status.decode("utf-8"),
        "files": files, "source_sha256": digest(encode(files)),
    }


def plan(mode, targets, test_filter="", step_timeout_seconds=1800, examples=()):
    if not isinstance(step_timeout_seconds, int) or not 1 <= step_timeout_seconds <= 7200:
        raise ValueError("Step timeout must be 1..7200 seconds")
    if mode not in ("check", "targeted", "full", "lib", "test-build", "example-build"):
        raise ValueError("Unknown mode")
    if len(targets) != len(set(targets)) or any(not re.fullmatch(r"[A-Za-z0-9_]+", t) for t in targets):
        raise ValueError("Targets must be distinct integration-test names")
    if test_filter and not re.fullmatch(r"[A-Za-z0-9_:]+", test_filter):
        raise ValueError("Invalid test filter")
    if len(examples) != len(set(examples)) or any(not re.fullmatch(r"[A-Za-z0-9_]+", t) for t in examples):
        raise ValueError("Examples must be distinct example names")
    if mode in ("targeted", "test-build") and not targets:
        raise ValueError("This mode requires --test-target")
    if mode == "check" and (targets or test_filter):
        raise ValueError("check mode has no test arguments")
    if test_filter and not targets and mode != "lib":
        raise ValueError("Filter requires an explicit test target")
    if mode == "lib" and (targets or not test_filter):
        raise ValueError("lib mode requires only an explicit --filter")
    if mode == "example-build" and (not examples or targets or test_filter):
        raise ValueError("example-build requires only explicit --example names")
    if examples and mode != "example-build":
        raise ValueError("Examples require example-build mode")
    if mode == "test-build" and test_filter:
        raise ValueError("test-build cannot run a filter")
    steps = [("check", ["cargo", "check", "--workspace", "--all-targets", "--offline", "--locked"])]
    if mode != "check":
        steps.append(("debug-bins", ["cargo", "build", "--bins", "--offline", "--locked"]))
        for target in targets:
            args = ["cargo", "test", "--offline", "--locked", "--test", target]
            if mode == "test-build":
                steps.append(("compile-test-" + target, args + ["--no-run"]))
                continue
            if test_filter:
                args.append(test_filter)
            args += ["--", "--test-threads=1"]
            steps.append(("target-" + target, args))
        if mode == "lib":
            steps.append(("library-test", ["cargo", "test", "--offline", "--locked", "--lib", test_filter,
                                         "--", "--test-threads=1"]))
        for example in examples:
            steps.append(("example-" + example, ["cargo", "build", "--offline", "--locked", "--example", example]))
    if mode == "full":
        steps += [
            ("test", ["pwsh", "-NoProfile", "-File", "build.ps1", "-Action", "test"]),
            ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
            ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--offline", "--locked", "--", "-D", "warnings"]),
            ("wasm-check", ["cargo", "check", "-p", "peachsh-ui", "--target", "wasm32-unknown-unknown", "--offline", "--locked"]),
        ]
    result = []
    for name, command in steps:
        if name == "library-test":
            stage = "lib-test"
        elif name.startswith("target-"):
            stage = "targeted"
        elif name.startswith("compile-test-"):
            stage = "test-build"
        elif name.startswith("example-"):
            stage = "example-build"
        else:
            stage = name
        argv = ["pwsh", "-NoProfile", "-File", "tools/lifecycle/invoke-step.ps1", "-Stage", stage]
        if stage == "targeted":
            argv += ["-TestTarget", name.removeprefix("target-")]
            if test_filter:
                argv += ["-Filter", test_filter]
        elif stage == "lib-test":
            argv += ["-Filter", test_filter]
        elif stage == "test-build":
            argv += ["-TestTarget", name.removeprefix("compile-test-")]
        elif stage == "example-build":
            argv += ["-Example", name.removeprefix("example-")]
        result.append({"name": name, "argv": argv, "command": command,
                       "timeout_seconds": step_timeout_seconds})
    return result


class Gate:
    """Persistent exclusive-create gate shared by every worktree of the repository.

    Never recover by PID or age. A failed/interrupted owner requires manager review
    and archival of this exact lock; there is intentionally no force-unlock option.
    """
    def __init__(self, path, record):
        self.path = path
        self.raw = encode({**record, "nonce": uuid.uuid4().hex})
        with path.open("xb") as stream:
            stream.write(self.raw)
            stream.flush()
            os.fsync(stream.fileno())

@dataclass(frozen=True)
class ExecutionResult:
    exit_code: int
    terminal_unknown: bool = False
    reason: str | None = None


def execute(argv, cwd, environment, timeout_seconds, log):
    # File-backed output avoids a pipe-reader lifetime being mistaken for EOF.
    # The deadline closes validation, not ownership. Once unknown, this SAME
    # supervisor keeps the original Popen handle and open log until that process
    # signals. It never relaunches/kills/reopens a PID. The global gate remains
    # after even ordinary exit: this is not a descendant cleanup proof.
    with log.open("xb") as stream:
        process = subprocess.Popen(argv, cwd=cwd, env=environment, stdin=subprocess.DEVNULL,
                                   stdout=stream, stderr=subprocess.STDOUT, creationflags=NO_WINDOW)
        deadline = time.monotonic() + timeout_seconds
        reason = None
        offset = 0
        tail = b""

        def mark_unknown(cause):
            nonlocal reason
            if reason is None:
                reason = cause
                try:
                    write_new(log.with_suffix(".terminal-unknown.json"), {
                        "reason": cause, "observed_utc": utc(), "timeout_seconds": timeout_seconds,
                        "original_process_handle_retained": True, "gate_retained": True,
                        "instruction": "Do not kill this supervisor; no further steps will start."
                    })
                    print(f"NHC_TERMINAL_UNKNOWN {log.name}: {cause}; original handle retained, waiting for direct terminal state", flush=True)
                except BaseException:
                    # A broken disk/stdout or another interrupt must not unwind
                    # the only owner of a still-live original process handle.
                    pass

        def read_marker(max_bytes=65536):
            nonlocal offset, tail
            with log.open("rb") as reader:
                reader.seek(offset)
                # Poll bounded chunks; no unbounded log allocation while waiting.
                chunk = reader.read(min(max_bytes, 65536))
                offset += len(chunk)
                text = tail + chunk
                tail = text[-len(b"NHC_TERMINAL_UNKNOWN"):]
                if b"NHC_TERMINAL_UNKNOWN" in text:
                    mark_unknown("child_terminal_unknown")
                return bool(chunk)

        while True:
            try:
                read_marker()
                if time.monotonic() >= deadline:
                    mark_unknown("step_deadline_exceeded")
                code = process.wait(timeout=0.25)
                # Also inspect final bytes from a fast exiting command.
                try:
                    final_size = log.stat().st_size
                    while offset < final_size and read_marker(final_size - offset):
                        pass
                    write_new(log.with_suffix(".direct-exit.json"), {
                        "exit_code": code, "observed_utc": utc(),
                        "terminal_unknown": reason is not None,
                        "process_tree_cleanup": "not_proven_by_this_runner",
                    })
                except BaseException:
                    mark_unknown("terminal_evidence_error")
                return ExecutionResult(code, reason is not None, reason)
            except subprocess.TimeoutExpired:
                continue
            except KeyboardInterrupt:
                mark_unknown("supervisor_interrupted")
            except BaseException:
                # Do not drop the original process handle on observation/log errors.
                # Preserve sticky unknown even when writing its sidecar failed.
                mark_unknown("supervision_error")
                try:
                    time.sleep(0.25)
                except BaseException:
                    pass


def validate_candidate(current, expected):
    if not current["clean"]:
        raise ValueError("Build inputs are dirty (tracked, staged or untracked)")
    for key in ("head", "product_tree", "rust_tree"):
        if current[key] != expected[key]:
            raise ValueError(f"Candidate {key} mismatch")


def run_attempt(repo, out, gate_path, expected, steps, environment, options,
                capture=snapshot, invoke=execute):
    if out.exists():
        raise FileExistsError("Attempt already exists; choose a new attempt name")
    gate = Gate(gate_path, {"attempt": os.path.relpath(out, repo), "candidate": expected,
                            "started_utc": utc(), "pid_diagnostic_only": os.getpid()})
    environment = dict(environment, NHC_LIFECYCLE_GATE_NONCE=json.loads(gate.raw)["nonce"],
                       NHC_LIFECYCLE_GATE_PATH=str(gate_path))
    manifest = {"version": 1, "candidate": expected, "options": options, "plan": steps,
                "started_utc": utc(), "steps": [], "status": "failed", "inputs_verified": False,
                "process_tree_cleanup": "not_proven_by_this_runner",
                "environment": {k: environment_value(environment, k) for k in [*ENVIRONMENT, "CARGO_TARGET_DIR", "TMP", "TEMP",
                    "VCToolsInstallDir", "WindowsSdkDir", "CC_x86_64_pc_windows_msvc", "AR_x86_64_pc_windows_msvc",
                    "CARGO_HOME", "RUSTUP_HOME", "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET"]}}
    code = 1
    try:
        out.mkdir(parents=True, exist_ok=False)
        before = capture(repo)
        write_new(out / "source-before.json", before)
        validate_candidate(before, expected)
        for index, step in enumerate(steps):
            if capture(repo) != before:
                raise ValueError("Inputs changed before step")
            record = {**step, "started_utc": utc(), "exit_code": None}
            prefix = f"{index + 1:02d}-{step['name']}"
            write_new(out / (prefix + ".started.json"), record)
            log = out / (prefix + ".log")
            outcome = invoke(step["argv"], repo / RUST_REL, environment, step["timeout_seconds"], log)
            code = 124 if outcome.terminal_unknown else outcome.exit_code
            raw = log.read_bytes()
            record.update(exit_code=outcome.exit_code, terminal_unknown=outcome.terminal_unknown,
                          unknown_reason=outcome.reason, ended_utc=utc(), log=log.name,
                          log_sha256=digest(raw), log_bytes=len(raw))
            manifest["steps"].append(record)
            write_new(out / (prefix + ".result.json"), record)
            if capture(repo) != before:
                raise ValueError("Inputs changed during step")
            if code:
                break
            if step["name"].startswith("target-") or step["name"] == "library-test":
                totals = re.findall(rb"test result: ok\. ([0-9]+) passed; 0 failed;", raw)
                if not totals or sum(int(n) for n in totals) == 0:
                    raise ValueError("Targeted test matched no passing tests; not a validated target")
        after = capture(repo)
        write_new(out / "source-after.json", after)
        manifest["inputs_verified"] = before == after
        if code == 0 and before == after and len(manifest["steps"]) == len(steps):
            manifest["status"] = "passed"
    except BaseException as error:
        code = 1
        manifest["error"] = f"{type(error).__name__}: {error}"
        if out.is_dir() and not (out / "source-after.json").exists():
            try:
                write_new(out / "source-after.json", capture(repo))
            except Exception as capture_error:
                write_new(out / "source-after-error.json", {"error": str(capture_error)})
    manifest["ended_utc"] = utc()
    if manifest["status"] != "passed" and code == 0:
        code = 1
    manifest["runner_exit_code"] = code
    manifest["gate_retained"] = True
    if out.is_dir():
        manifest["artifacts"] = [
            {"path": path.name, "bytes": len(raw), "sha256": digest(raw)}
            for path in sorted(out.iterdir()) if path.is_file()
            for raw in [path.read_bytes()]
        ]
        write_new(out / "manifest.json", manifest)
    return code, manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("head", "product-tree", "rust-tree", "attempt"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--mode", choices=("check", "targeted", "full", "lib", "test-build", "example-build"), default="full")
    parser.add_argument("--test-target", action="append", default=[])
    parser.add_argument("--example", action="append", default=[])
    parser.add_argument("--filter", default="")
    parser.add_argument("--evidence-root", default=".local/lifecycle")
    parser.add_argument("--target-dir", default=".local/lifecycle-target")
    parser.add_argument("--step-timeout-seconds", type=int, default=1800)
    args = parser.parse_args()
    expected = {k: getattr(args, k) for k in ("head", "product_tree", "rust_tree")}
    if any(not re.fullmatch(r"[0-9a-f]{40}", v) for v in expected.values()):
        parser.error("Candidate requires three full lower-case Git SHA-1 IDs")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", args.attempt):
        parser.error("Invalid attempt name")
    repo = e_path(REPO, ".")
    out = e_path(repo, args.evidence_root) / args.attempt
    target = e_path(repo, args.target_dir)
    temp = e_path(repo, str(out / "tmp"))
    # Keep generated files out of product inputs, including ignored directories.
    for path in (out, target, temp):
        if path.is_relative_to(repo / "NoManCode"):
            raise ValueError("Outputs must be outside NoManCode")
    common = e_path(repo, git_text(repo, "rev-parse", "--git-common-dir"))
    gate_path = common / "nhc-heavy-window.lock"
    steps = plan(args.mode, args.test_target, args.filter, args.step_timeout_seconds, args.example)
    for name in args.test_target:
        if not (repo / RUST_REL / "tests" / (name + ".rs")).is_file():
            raise ValueError(f"Integration target missing: {name}")
    for name in args.example:
        if not (repo / RUST_REL / "examples" / (name + ".rs")).is_file():
            raise ValueError(f"Example source missing: {name}")
    # Create output paths only while owning the global gate (inside first step).
    environment = dict(os.environ, **ENVIRONMENT, CARGO_TARGET_DIR=str(target), TMP=str(temp), TEMP=str(temp))

    def invoke(argv, cwd, env, timeout_seconds, log):
        target.mkdir(parents=True, exist_ok=True)
        temp.mkdir(parents=True, exist_ok=True)
        # Legacy test fixtures explicitly call tempdir_in("target"). This is
        # an empty fixture parent, NOT the Cargo build artifact target.
        (repo / RUST_REL / "target").mkdir(exist_ok=True)
        return execute(argv, cwd, env, timeout_seconds, log)

    code, manifest = run_attempt(repo, out, gate_path, expected, steps, environment,
                                {"mode": args.mode, "test_targets": args.test_target, "filter": args.filter,
                                 "step_timeout_seconds": args.step_timeout_seconds, "examples": args.example},
                                invoke=invoke)
    print(json.dumps({"status": manifest["status"], "exit_code": code, "evidence": str(out),
                      "gate_retained": manifest["gate_retained"]}))
    return code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"lifecycle gates: {error}", file=sys.stderr)
        sys.exit(1)
