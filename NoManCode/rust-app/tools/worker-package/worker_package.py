"""Prepare a new worker/Host pair. No installation, current-pointer swap or execution.

The worker copy is frozen before its SHA256 enters only the Host build process.
This records compile inputs; runtime identity/protocol acceptance is a separate gate.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lifecycle"))
from lifecycle_gates import (  # noqa: E402 - sibling product tooling, not a package install
    ENVIRONMENT, REPO, RUST_REL, Gate, digest, e_path, execute, git_text,
    snapshot, utc, validate_candidate, write_new,
)

DIGEST_VARIABLE = "NHC_PACKAGED_WORKER_SHA256"
HOST_SOURCE = RUST_REL + "/src/main.rs"
WORKER_SOURCE = RUST_REL + "/src/bin/peachsh-wasm-worker.rs"
PROTOCOL_SOURCE = RUST_REL + "/src/wasm_worker_protocol.rs"
TARGET = "x86_64-pc-windows-msvc"


def rust_tokens(text):
    """Lex just enough to exclude comments/literals from the macro-site check."""
    tokens = []
    index = 0
    while index < len(text):
        if text[index].isspace():
            index += 1
            continue
        if text.startswith("//", index):
            end = text.find("\n", index)
            index = len(text) if end < 0 else end + 1
            continue
        if text.startswith("/*", index):
            depth = 1
            index += 2
            while depth and index < len(text):
                if text.startswith("/*", index):
                    depth += 1
                    index += 2
                elif text.startswith("*/", index):
                    depth -= 1
                    index += 2
                else:
                    index += 1
            continue
        raw = re.match(r'(?:br|cr|r)(#*)"', text[index:])
        if raw:
            ending = '"' + raw.group(1)
            end = text.find(ending, index + raw.end())
            index = len(text) if end < 0 else end + len(ending)
            tokens.append(("literal", ""))
            continue
        if text[index] == '"':
            start = index
            index += 1
            while index < len(text):
                if text[index] == "\\":
                    index += 2
                elif text[index] == '"':
                    index += 1
                    break
                else:
                    index += 1
            tokens.append(("string", text[start:index]))
            continue
        number = re.match(r'[0-9]+', text[index:])
        if number:
            tokens.append(("number", number.group()))
            index += number.end()
            continue
        identifier = re.match(r'[A-Za-z_][A-Za-z0-9_]*', text[index:])
        if identifier:
            tokens.append(("identifier", identifier.group()))
            index += identifier.end()
        else:
            tokens.append(("punctuation", text[index]))
            index += 1
    return tokens


def validate_binding_source(repo, source):
    """Require the agreed Host-only compile input before invoking any compiler.

    This is a lexical source contract check, not macro expansion, Rust type
    checking, proof of use in bootstrap, or runtime attestation.
    """
    consumers = []
    for entry in source["files"]:
        name = entry["path"]
        if not name.endswith(".rs"):
            continue
        text = (repo / name).read_text(encoding="utf-8-sig")
        tokens = rust_tokens(text)
        for index in range(len(tokens) - 4):
            if (tokens[index][1] in ("option_env", "env") and tokens[index + 1][1] == "!"
                    and tokens[index + 2][1] == "(" and tokens[index + 3] == ("string", f'"{DIGEST_VARIABLE}"')):
                consumers.append((name, tokens[index][1]))
    if consumers != [(HOST_SOURCE, "option_env")]:
        raise ValueError("Host-only option_env digest consumer missing or duplicated; bootstrap is not ready")


def verify_pair(path, expected):
    record = json.loads((path / "pair.json").read_bytes())
    if record["candidate"] != expected or record["runtime_binding_verified"] is not False:
        raise ValueError("Pair candidate or validation scope mismatch")
    if {item.name for item in path.iterdir()} != {"pair.json", "peachsh.exe", "peachsh-wasm-worker.exe"}:
        raise ValueError("Unexpected or missing file in prepared pair")
    for name, file in (("worker", "peachsh-wasm-worker.exe"), ("host", "peachsh.exe")):
        if record[name]["file"] != file:
            raise ValueError("Pair filenames are fixed")
        verify_frozen(path, record[name])
    if record["host_compile_input"] != {DIGEST_VARIABLE: record["worker"]["sha256"]}:
        raise ValueError("Host compile input does not match the frozen worker")
    return record


def protocol_version(repo):
    tokens = rust_tokens((repo / PROTOCOL_SOURCE).read_text(encoding="utf-8-sig"))
    code = " ".join(value for kind, value in tokens if kind not in ("literal", "string"))
    matches = re.findall(r"\bpub const VERSION : u32 = (\d+) ;", code)
    if len(matches) != 1 or not 1 <= int(matches[0]) <= 0xFFFFFFFF:
        raise ValueError("Cannot bind the protocol VERSION source constant")
    return int(matches[0])


def validate_layout(evidence, stage, pair, target_dir):
    if stage.parent.resolve() != pair.parent.resolve():
        raise ValueError("Staging and final pair must share a filesystem parent")
    paths = [p.resolve() for p in (evidence, stage, pair, target_dir)]
    for index, path in enumerate(paths):
        for other in paths[index + 1:]:
            if path.is_relative_to(other) or other.is_relative_to(path):
                raise ValueError("Evidence, staging, pair and mutable target must not overlap")


def cargo_command(stage, profile, target):
    if stage not in ("worker", "host") or profile not in ("debug", "release") or target != TARGET:
        raise ValueError("Unsupported build selection")
    name = "peachsh-wasm-worker" if stage == "worker" else "peachsh"
    args = ["cargo", "build", "--offline", "--locked", "--target", target, "--bin", name,
            "--message-format=json-render-diagnostics"]
    if profile == "release":
        args.append("--release")
    return args


def select_artifact(raw, repo, stage, target_dir):
    expected_name = "peachsh-wasm-worker" if stage == "worker" else "peachsh"
    expected_source = (repo / (WORKER_SOURCE if stage == "worker" else HOST_SOURCE)).resolve()
    found = set()
    finished = []
    for line in raw.splitlines():
        try:
            message = json.loads(line)
        except (ValueError, UnicodeDecodeError):
            continue  # Rust diagnostics are also retained in this raw log.
        if not isinstance(message, dict):
            continue
        if message.get("reason") == "build-finished":
            finished.append(message.get("success"))
        target = message.get("target", {})
        if (message.get("reason") != "compiler-artifact" or target.get("name") != expected_name
                or target.get("kind") != ["bin"] or message.get("profile", {}).get("test") is not False
                or not target.get("src_path") or not message.get("executable")):
            continue
        source = (repo / RUST_REL / target["src_path"]).resolve()
        if source != expected_source:
            continue
        path = (repo / RUST_REL / message["executable"]).resolve()
        if not path.is_relative_to(target_dir.resolve()) or path.drive.upper() != "E:":
            raise ValueError("Cargo artifact escaped the explicitly selected target directory")
        found.add(path)
    if finished != [True] or len(found) != 1:
        raise ValueError("Need one matching artifact and one successful build-finished from this invocation")
    path = found.pop()
    if not path.is_file():
        raise ValueError("This invocation's executable artifact is missing")
    return path


def freeze(artifact, destination):
    # Independent bytes, never a hard link into mutable Cargo target storage.
    raw = artifact.read_bytes()
    if not raw:
        raise ValueError("Empty executable artifact")
    with destination.open("xb") as stream:
        stream.write(raw)
        stream.flush()
        os.fsync(stream.fileno())
    sha = digest(raw)
    if digest(artifact.read_bytes()) != sha or digest(destination.read_bytes()) != sha:
        raise ValueError("Artifact changed while freezing")
    if destination.stat().st_nlink != 1:
        raise ValueError("Frozen artifact must have a single independent file link")
    return {"file": destination.name, "sha256": sha, "bytes": len(raw)}


def verify_frozen(stage, record):
    path = stage / record["file"]
    raw = path.read_bytes()
    if path.is_symlink() or path.stat().st_nlink != 1 or len(raw) != record["bytes"] or digest(raw) != record["sha256"]:
        raise ValueError("Frozen pair artifact changed")


def build_pair(repo, evidence, stage, pair, gate_path, expected, target_dir, profile, target,
               timeout_seconds, environment, capture=snapshot, invoke=execute):
    if profile not in ("debug", "release") or target != TARGET or not 1 <= timeout_seconds <= 7200:
        raise ValueError("Unsupported profile, target or timeout")
    validate_layout(evidence, stage, pair, target_dir)
    for path in (evidence, stage, pair):
        if path.exists():
            raise FileExistsError(f"Refusing to replace existing attempt, staging or pair: {path}")
    gate = Gate(gate_path, {"kind": "worker-pair", "candidate": expected,
                            "attempt": os.path.relpath(evidence, repo), "started_utc": utc()})
    env = dict(environment, **ENVIRONMENT, CARGO_TARGET_DIR=str(target_dir),
               TMP=str(evidence / "tmp"), TEMP=str(evidence / "tmp"),
               NHC_LIFECYCLE_GATE_PATH=str(gate_path), NHC_LIFECYCLE_GATE_NONCE=json.loads(gate.raw)["nonce"])
    # These are scoped compile inputs, never inherited from an interactive shell.
    for key in (DIGEST_VARIABLE, "NHC_PACKAGED_WORKER_PROTOCOL"):
        env.pop(key, None)
    report = {"version": 1, "candidate": expected, "started_utc": utc(), "status": "failed",
              "profile": profile, "target": target, "features": "Cargo defaults; identical for both bins",
              "steps": [], "gate_retained": True, "process_tree_cleanup": "not_proven_by_this_tool",
              "runtime_binding_verified": False}
    # Record only relevant non-secret configuration, never dump the caller's env.
    report["environment"] = {key: env.get(key) for key in (
        *ENVIRONMENT, "CARGO_TARGET_DIR", "TMP", "TEMP", "VCToolsInstallDir",
        "VSINSTALLDIR", "WindowsSdkDir", "UniversalCRTSdkDir",
    )}
    code = 1
    try:
        evidence.mkdir(parents=True, exist_ok=False)
        before = capture(repo)
        write_new(evidence / "source-before.json", before)
        validate_candidate(before, expected)
        validate_binding_source(repo, before)
        protocol = protocol_version(repo)
        write_new(evidence / "preflight.json", {
            "status": "passed", "host_digest_consumer": HOST_SOURCE,
            "compile_variable": DIGEST_VARIABLE, "protocol_version": protocol,
            "scope": "lexical source contract; not runtime binding proof",
        })
        stage.mkdir(parents=True, exist_ok=False)
        target_dir.mkdir(parents=True, exist_ok=True)
        (evidence / "tmp").mkdir()
        frozen = {}
        for number, kind in enumerate(("worker", "host"), 1):
            if capture(repo) != before:
                raise ValueError("Source changed between builds")
            invocation_env = env.copy()
            if kind == "host":
                verify_frozen(stage, frozen["worker"])
                invocation_env[DIGEST_VARIABLE] = frozen["worker"]["sha256"]
            argv = ["pwsh", "-NoProfile", "-File", "tools/worker-package/invoke-build.ps1",
                    "-Stage", kind, "-Profile", profile, "-Target", target]
            record = {"stage": kind, "argv": argv, "command": cargo_command(kind, profile, target),
                      "timeout_seconds": timeout_seconds, "started_utc": utc(), "exit_code": None,
                      "host_compile_digest": invocation_env.get(DIGEST_VARIABLE)}
            prefix = f"{number:02d}-{kind}"
            write_new(evidence / (prefix + ".started.json"), record)
            log = evidence / (prefix + ".log")
            outcome = invoke(argv, repo / RUST_REL, invocation_env, timeout_seconds, log)
            raw = log.read_bytes()
            record.update(exit_code=outcome.exit_code, terminal_unknown=outcome.terminal_unknown,
                          reason=outcome.reason, ended_utc=utc(), log=log.name,
                          log_sha256=digest(raw), log_bytes=len(raw))
            report["steps"].append(record)
            write_new(evidence / (prefix + ".result.json"), record)
            if outcome.terminal_unknown or outcome.exit_code:
                code = 124 if outcome.terminal_unknown else outcome.exit_code
                raise ValueError(f"{kind} build did not finish with a known successful result")
            if capture(repo) != before:
                raise ValueError("Source changed during build")
            artifact = select_artifact(raw, repo, kind, target_dir)
            filename = "peachsh-wasm-worker.exe" if kind == "worker" else "peachsh.exe"
            frozen[kind] = freeze(artifact, stage / filename)
            write_new(evidence / (prefix + ".frozen.json"), frozen[kind])
        for record in frozen.values():
            verify_frozen(stage, record)
        after = capture(repo)
        write_new(evidence / "source-after.json", after)
        if before != after:
            raise ValueError("Final source snapshot changed")
        pair_record = {"version": 1, "candidate": expected, "source_sha256": before["source_sha256"],
                       "profile": profile, "target": target, "features": report["features"],
                       "worker": frozen["worker"], "host": frozen["host"],
                       "host_compile_input": {DIGEST_VARIABLE: frozen["worker"]["sha256"]},
                       "protocol_version": protocol, "protocol_source": PROTOCOL_SOURCE,
                       "runtime_binding_verified": False,
                       "scope": "prepared pair only; no installation or public run_wasm enablement"}
        write_new(stage / "pair.json", pair_record)
        verify_pair(stage, expected)
        # Same parent filesystem, no existing destination, Windows rename does
        # not replace a concurrent destination. No current/installed pair touched.
        stage.rename(pair)
        report.update(status="prepared", pair=os.path.relpath(pair, repo), pair_sha256=digest((pair / "pair.json").read_bytes()))
        code = 0
    except BaseException as error:
        report["error"] = f"{type(error).__name__}: {error}"
        if evidence.is_dir() and not (evidence / "preflight.json").exists():
            write_new(evidence / "preflight.json", {"status": "failed", "error": report["error"],
                                                    "build_started": False})
        if evidence.is_dir() and not (evidence / "source-after.json").exists():
            try:
                write_new(evidence / "source-after.json", capture(repo))
            except Exception as capture_error:
                write_new(evidence / "source-after-error.json", {"error": str(capture_error)})
    report.update(ended_utc=utc(), runner_exit_code=code)
    if evidence.is_dir():
        report["artifacts"] = [{"file": p.name, "sha256": digest(p.read_bytes()), "bytes": p.stat().st_size}
                               for p in sorted(evidence.iterdir()) if p.is_file()]
        write_new(evidence / "manifest.json", report)
    return code, report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("head", "product-tree", "rust-tree", "pair"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--profile", choices=("debug", "release"), default="debug")
    parser.add_argument("--target", choices=(TARGET,), default=TARGET)
    parser.add_argument("--package-root", default=".local/worker-packages")
    parser.add_argument("--evidence-root", default=".local/worker-package-evidence")
    parser.add_argument("--target-dir", default=".local/lifecycle-target")
    parser.add_argument("--step-timeout-seconds", type=int, default=1800)
    args = parser.parse_args()
    expected = {key: getattr(args, key) for key in ("head", "product_tree", "rust_tree")}
    if any(not re.fullmatch(r"[0-9a-f]{40}", value) for value in expected.values()):
        parser.error("Full candidate SHAs are required")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", args.pair):
        parser.error("Invalid pair name")
    repo = e_path(REPO, ".")
    packages = e_path(repo, args.package_root)
    evidence = e_path(repo, args.evidence_root) / args.pair
    target_dir = e_path(repo, args.target_dir)
    stage, pair = packages / (".staging-" + args.pair), packages / args.pair
    for path in (packages, evidence, target_dir):
        if path.is_relative_to(repo / "NoManCode"):
            raise ValueError("Pair, target and evidence must be outside product inputs")
    if target_dir.is_relative_to(packages) or packages.is_relative_to(target_dir):
        raise ValueError("Mutable Cargo output must be separate from frozen packages")
    gate = e_path(repo, git_text(repo, "rev-parse", "--git-common-dir")) / "nhc-heavy-window.lock"
    code, report = build_pair(repo, evidence, stage, pair, gate, expected, target_dir,
                              args.profile, args.target, args.step_timeout_seconds, dict(os.environ))
    print(json.dumps({"status": report["status"], "evidence": str(evidence),
                      "gate_retained": True, "runtime_binding_verified": False}))
    return code


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        print(f"worker pair: {error}", file=sys.stderr)
        sys.exit(1)
