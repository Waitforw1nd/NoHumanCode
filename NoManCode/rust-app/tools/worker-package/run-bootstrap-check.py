"""Manager-only real pair check, supervised by the shared lifecycle gate."""
import argparse
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lifecycle"))
from lifecycle_gates import ENVIRONMENT, REPO, ExecutionResult, e_path, execute, git_text, run_attempt


def exactly_one_passed(raw):
    summaries = re.findall(rb"(?m)^test result: ok\. (\d+) passed; (\d+) failed;[^\r\n]*", raw)
    return summaries == [(b"1", b"0")]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("head", "product-tree", "rust-tree", "attempt"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--target-dir", default=".local/lifecycle-target")
    parser.add_argument("--evidence-root", default=".local/lifecycle")
    parser.add_argument("--step-timeout-seconds", type=int, default=300)
    args = parser.parse_args()
    expected = {k: getattr(args, k) for k in ("head", "product_tree", "rust_tree")}
    if any(not re.fullmatch(r"[0-9a-f]{40}", v) for v in expected.values()):
        parser.error("full candidate SHA-1 IDs required")
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,79}", args.attempt):
        parser.error("invalid attempt")
    if not 1 <= args.step_timeout_seconds <= 7200:
        parser.error("invalid timeout")
    repo = e_path(REPO, ".")
    out = e_path(repo, args.evidence_root) / args.attempt
    target = e_path(repo, args.target_dir)
    if out.is_relative_to(repo / "NoManCode") or target.is_relative_to(repo / "NoManCode"):
        raise ValueError("outputs must be outside product inputs")
    if out == target or out.is_relative_to(target) or target.is_relative_to(out):
        raise ValueError("target and evidence must not overlap")
    env = dict(os.environ, **ENVIRONMENT, CARGO_TARGET_DIR=str(target),
               TMP=str(out / "tmp"), TEMP=str(out / "tmp"))
    for key in ("NHC_TEST_PAIR_A", "NHC_TEST_FOREIGN_WORKER", "NHC_TEST_UNBOUND_HOST"):
        if not env.get(key):
            raise ValueError(f"missing {key}")
        env[key] = str(e_path(repo, env[key]))
    common = e_path(repo, git_text(repo, "rev-parse", "--git-common-dir"))
    argv = ["pwsh", "-NoProfile", "-File", "tools/worker-package/invoke-bootstrap-check.ps1"]
    steps = [{"name": "target-real-pair", "argv": argv,
              "command": ["cargo", "test", "--offline", "--locked", "--test", "worker_package_bootstrap",
                          "--", "real_pair_host_check_rejects_tampering_and_mix", "--exact", "--ignored",
                          "--nocapture", "--test-threads=1"], "timeout_seconds": args.step_timeout_seconds}]

    def invoke(argv, cwd, environment, timeout, log):
        target.mkdir(parents=True, exist_ok=True)
        (out / "tmp").mkdir()
        result = execute(argv, cwd, environment, timeout, log)
        if not result.terminal_unknown and result.exit_code == 0 and not exactly_one_passed(log.read_bytes()):
            return ExecutionResult(1, False, "expected exactly one passing test")
        return result

    code, report = run_attempt(repo, out, common / "nhc-heavy-window.lock", expected, steps, env,
                               {"kind": "real-worker-pair-check"}, invoke=invoke)
    print(f"status={report['status']} evidence={out} gate_retained=true", flush=True)
    return code


if __name__ == "__main__":
    sys.exit(main())
