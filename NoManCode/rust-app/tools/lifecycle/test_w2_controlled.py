"""Pure mock builds and processes; no real Cargo, worker, or global gate."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import w2_controlled as runner


class ControlledTests(unittest.TestCase):
    def setUp(self):
        root = runner.REPO / ".local/w2-mocks"
        root.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=root)
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.target = self.repo / ".local/lifecycle-w2-target"
        self.source = self.repo / runner.RUST_REL
        path = self.source / "tests/support/wasm_worker_process/prepare-probes.ps1"
        path.parent.mkdir(parents=True)
        path.write_text("$modes = @('control','no-read')", encoding="utf-8")

    def artifact(self, key, path):
        name, kind, source, test = runner.ARTIFACTS[key]
        return {"reason": "compiler-artifact", "target": {"name": name, "kind": kind,
                "src_path": str(self.source / source)}, "profile": {"test": test}, "executable": str(path)}

    def test_select_artifacts_requires_unique_matching_json_and_finish(self):
        path = self.target / "worker.exe"
        good = self.artifact("worker", path)
        def log(*items):
            return b"\n".join(json.dumps(item).encode() for item in items)
        finish = {"reason": "build-finished", "success": True}
        self.assertEqual(runner.select_artifacts(log(good, finish), self.repo, self.target, ("worker",))["worker"], path)
        other = self.artifact("worker", self.target / "another.exe")
        for raw in (log(finish), log(good), log(good, other, finish), log(good, finish, finish)):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                runner.select_artifacts(raw, self.repo, self.target, ("worker",))
        escaped = self.artifact("worker", self.repo / "escaped.exe")
        with self.assertRaises(ValueError):
            runner.select_artifacts(log(escaped, finish), self.repo, self.target, ("worker",))

    def test_exact_one_rejects_zero_multiple_and_unestablished(self):
        self.assertTrue(runner.exact_one(b"test result: ok. 1 passed; 0 failed; 0 ignored\n"))
        for text in (b"test result: ok. 0 passed; 0 failed;\n", b"test result: ok. 2 passed; 0 failed;\n",
                     b"test result: ok. 1 passed; 0 failed;\ntest result: ok. 1 passed; 0 failed;\n",
                     b"not established\ntest result: ok. 1 passed; 0 failed;\n"):
            self.assertFalse(runner.exact_one(text))

    def test_listing_rejects_missing_and_new_unmapped_cases(self):
        names = (*runner.CASES, *runner.HELPERS)
        self.assertIn(runner.PREFIX + "w07_bound_shutdown_before_resume_retains_ticket", names)
        self.assertIn(runner.PREFIX + "w07_bound_shutdown_running_retains_ticket", names)
        raw = "\n".join(name + ": test" for name in names).encode()
        self.assertTrue(runner.listing_matches(raw))
        self.assertFalse(runner.listing_matches(b"\n".join(raw.splitlines()[1:])))
        self.assertFalse(runner.listing_matches(raw + b"\nwasm_worker_process_tests::unexpected: test\n"))

    def test_mock_matrix_prepares_fixed_copies_and_separate_children(self):
        calls = []
        cases = [runner.CASES[0], runner.CASES[-1]]
        args = ["runner", "--head", "a" * 40, "--product-tree", "b" * 40, "--rust-tree", "c" * 40,
                "--attempt", "mock", "--case", cases[0], "--case", cases[1], "--step-timeout-seconds", "20"]

        def fake_execute(argv, cwd, env, timeout, log, unknown_files=()):
            calls.append((argv, env.copy(), unknown_files))
            if "-Stage" in argv:
                stage = argv[-1]
                keys = ("worker", "probe") if stage == "bins" else ("lib-test",)
                messages = []
                for key in keys:
                    path = self.target / (key + ".exe")
                    path.write_bytes((key + " bytes").encode())
                    messages.append(self.artifact(key, path))
                messages.append({"reason": "build-finished", "success": True})
                log.write_bytes(b"\n".join(json.dumps(m).encode() for m in messages))
            elif "--list" in argv:
                log.write_text("\n".join(name + ": test" for name in (*runner.CASES, *runner.HELPERS)), encoding="utf-8")
            else:
                self.assertEqual(len(unknown_files), 5)
                self.assertEqual({p.name for p, _ in unknown_files},
                                 {"host.stage", "before-host.stage", "suspended-host.stage", "resumed-host.stage", "pending-host.stage"})
                self.assertEqual(Path(env["NHC_W2_WORKER_SNAPSHOT"]).read_bytes(), b"worker bytes")
                self.assertEqual(Path(env["NHC_W2_FAULT_DIR"], "no-read.exe").read_bytes(), b"probe bytes")
                self.assertNotIn("NHC_PACKAGED_WORKER_SHA256", env)
                log.write_bytes(b"test result: ok. 1 passed; 0 failed; 0 ignored;\n")
            return runner.ExecutionResult(0)

        def fake_run(repo, out, gate, expected, steps, env, options, invoke):
            self.assertEqual(gate.name, "nhc-heavy-window.lock")
            self.assertEqual(options["mapped_count"], 27)
            out.mkdir(parents=True)
            for index, step in enumerate(steps):
                result = invoke(step["argv"], self.source, env, step["timeout_seconds"], out / f"{index}.log")
                if result.exit_code or result.terminal_unknown:
                    return 1, {"status": "failed"}
            return 0, {"status": "passed"}

        with patch.object(runner, "REPO", self.repo), patch.object(runner, "git_text", return_value=str(self.repo)), \
             patch.object(runner, "run_attempt", side_effect=fake_run), patch.object(runner, "execute", side_effect=fake_execute), \
             patch.object(sys, "argv", args), patch.dict(os.environ, {"NHC_PACKAGED_WORKER_SHA256": "bad"}):
            self.assertEqual(runner.main(), 0)
        self.assertEqual(len(calls), 5)
        self.assertNotEqual(calls[3][1]["TMP"], calls[4][1]["TMP"])
        self.assertEqual(calls[3][0][2], cases[0])
        self.assertEqual(calls[4][0][2], cases[1])


if __name__ == "__main__":
    unittest.main(verbosity=2)
