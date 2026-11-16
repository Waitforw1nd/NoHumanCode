"""Verify a completed gate attempt, then copy its exact bytes to a new directory.

No normalization of raw evidence, replacement, implied process cleanup or automatic
acceptance of a later HEAD. Failed attempts stay in their original attempt directory.
"""
import argparse
import json
import os
from pathlib import Path
import sys

from lifecycle_gates import REPO, ENVIRONMENT, digest, e_path, encode, plan, snapshot, utc, validate_candidate, write_new


def verify(attempt, current):
    # Read once and hash/copy these same byte snapshots, avoiding re-read drift.
    blobs = {p.name: p.read_bytes() for p in sorted(attempt.iterdir()) if p.is_file()}
    manifest = json.loads(blobs["manifest.json"])
    if manifest["version"] != 1 or manifest["status"] != "passed" or not manifest["inputs_verified"]:
        raise ValueError("Attempt is not a completed passing gate")
    if manifest["process_tree_cleanup"] != "not_proven_by_this_runner" or not manifest["gate_retained"]:
        raise ValueError("Unsupported process cleanup claim")
    options = manifest["options"]
    expected_plan = plan(options["mode"], options["test_targets"], options["filter"], options["step_timeout_seconds"])
    if manifest["plan"] != expected_plan or len(manifest["steps"]) != len(expected_plan):
        raise ValueError("Incomplete or substituted plan")
    for key, value in ENVIRONMENT.items():
        if manifest["environment"].get(key) != value:
            raise ValueError(f"Unexpected environment: {key}")
    before = json.loads(blobs["source-before.json"])
    after = json.loads(blobs["source-after.json"])
    validate_candidate(before, manifest["candidate"])
    validate_candidate(current, manifest["candidate"])
    if before != after or before != current or before["source_sha256"] != digest(encode(before["files"])):
        raise ValueError("Source hashes or current candidate differ from tested inputs")
    for index, (step, required) in enumerate(zip(manifest["steps"], expected_plan)):
        prefix = f"{index + 1:02d}-{required['name']}"
        if any(step[key] != required[key] for key in required) or step["exit_code"] != 0 or step["terminal_unknown"]:
            raise ValueError("Step did not pass the exact planned command")
        if step["log"] != prefix + ".log":
            raise ValueError("Unexpected log path")
        raw = blobs[step["log"]]
        if len(raw) != step["log_bytes"] or digest(raw) != step["log_sha256"]:
            raise ValueError("Log hash or length mismatch")
        if json.loads(blobs[prefix + ".result.json"]) != step:
            raise ValueError("Step result differs from final manifest")
        started = json.loads(blobs[prefix + ".started.json"])
        if started != {**required, "started_utc": step["started_utc"], "exit_code": None}:
            raise ValueError("Step start differs from result")
    return manifest, blobs


def publish(attempt, destination, current):
    manifest, blobs = verify(attempt, current)
    destination.mkdir(parents=True, exist_ok=False)
    records = []
    for name, raw in blobs.items():
        with (destination / name).open("xb") as stream:
            stream.write(raw)
        records.append({"path": name, "source_sha256": digest(raw), "published_sha256": digest(raw), "bytes": len(raw)})
    write_new(destination / "publication.json", {
        "verified_utc": utc(), "candidate": manifest["candidate"],
        "mode": manifest["options"]["mode"],
        "full_gate_suite": manifest["options"]["mode"] == "full",
        "source_attempt": os.path.relpath(attempt, destination),
        "process_tree_cleanup": "not_proven_by_this_runner", "files": records,
    })


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--attempt-dir", required=True, help="Repository-relative path")
    parser.add_argument("--destination", required=True, help="New repository-relative directory")
    args = parser.parse_args()
    attempt = e_path(REPO, args.attempt_dir)
    destination = e_path(REPO, args.destination)
    if destination.is_relative_to(REPO / "NoManCode") or destination.is_relative_to(attempt):
        raise ValueError("Publication must be outside product inputs and the attempt")
    current = snapshot(REPO)
    publish(attempt, destination, current)
    # A concurrent edit during publication cannot gain a verified publication seal.
    if snapshot(REPO) != current:
        write_new(destination / "INVALIDATED.json", {"reason": "Inputs changed during publication"})
        raise ValueError("Inputs changed during publication; INVALIDATED marker retained")
    print(str(destination))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"publish evidence: {error}", file=sys.stderr)
        sys.exit(1)
