"""Test the runner's plan only; no child process or Cargo is started."""
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("bootstrap_runner", Path(__file__).with_name("run-bootstrap-check.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class RunnerTests(unittest.TestCase):
    def test_real_pair_plan_uses_gate_and_exact_ignored_test(self):
        root = runner.REPO / ".local/worker-package-mocks"
        root.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=root) as temp:
            repo = Path(temp)
            args = ["runner", "--head", "a" * 40, "--product-tree", "b" * 40,
                    "--rust-tree", "c" * 40, "--attempt", "fixture", "--step-timeout-seconds", "300"]
            env = {"NHC_TEST_PAIR_A": str(repo / "pair"),
                   "NHC_TEST_FOREIGN_WORKER": str(repo / "worker.exe"),
                   "NHC_TEST_UNBOUND_HOST": str(repo / "host.exe")}
            captured = []

            def fake_run(*args, **kwargs):
                captured.append((args, kwargs))
                return 0, {"status": "passed"}

            with mock.patch.object(runner, "REPO", repo), mock.patch.object(runner, "git_text", return_value=str(repo)), \
                 mock.patch.object(runner, "run_attempt", side_effect=fake_run), \
                 mock.patch.object(sys, "argv", args), mock.patch.dict(os.environ, env):
                self.assertEqual(runner.main(), 0)
            args, kwargs = captured[0]
            self.assertEqual(args[2].name, "nhc-heavy-window.lock")
            step = args[4][0]
            self.assertEqual(step["timeout_seconds"], 300)
            self.assertIn("--ignored", step["command"])
            self.assertIn("--exact", step["command"])
            self.assertIn("--nocapture", step["command"])
            self.assertEqual(args[5]["NHC_TEST_FOREIGN_WORKER"], env["NHC_TEST_FOREIGN_WORKER"])
            self.assertIn("invoke", kwargs)


if __name__ == "__main__":
    unittest.main(verbosity=2)
