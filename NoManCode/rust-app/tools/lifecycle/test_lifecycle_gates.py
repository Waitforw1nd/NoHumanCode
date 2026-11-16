"""Pure mocked runner tests: no Cargo, product processes, or real global gate."""
import copy
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import lifecycle_gates as gates
import publish_evidence as evidence


class GateTests(unittest.TestCase):
    def setUp(self):
        root = gates.REPO / ".local" / "lifecycle-mock-tests"
        root.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=root)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.out = self.root / "attempt"
        self.lock = self.root / "mock-global.lock"
        self.expected = {"head": "a" * 40, "product_tree": "b" * 40, "rust_tree": "c" * 40}
        files = [{"path": "NoManCode/source.rs", "bytes": 6, "sha256": gates.digest(b"source")}]
        self.source = {**self.expected, "clean": True, "status": "", "files": files, "source_sha256": gates.digest(gates.encode(files))}
        self.calls = []
        self.options = {"mode": "full", "test_targets": [], "filter": "", "step_timeout_seconds": 1800, "examples": []}

    def invoke(self, argv, cwd, env, timeout_seconds, log):
        self.assertTrue(self.lock.exists(), "No command before exclusive gate")
        self.calls.append(argv)
        log.write_bytes(b"mock: test result: ok. 2 passed; 0 failed; 0 ignored;\n")
        return gates.ExecutionResult(0)

    def run_attempt(self, **overrides):
        values = dict(repo=self.root, out=self.out, gate_path=self.lock, expected=self.expected,
                      steps=gates.plan(**{"mode": self.options["mode"], "targets": self.options["test_targets"], "test_filter": self.options["filter"]}),
                      environment=gates.ENVIRONMENT, options=self.options,
                      capture=lambda _: copy.deepcopy(self.source), invoke=self.invoke)
        values.update(overrides)
        return gates.run_attempt(**values)

    def test_full_success_and_exact_byte_publication(self):
        code, manifest = self.run_attempt()
        self.assertEqual(code, 0)
        self.assertEqual(len(manifest["steps"]), 6)
        self.assertTrue(self.lock.exists())
        destination = self.root / "published"
        evidence.publish(self.out, destination, self.source)
        for name in ("manifest.json", "01-check.log", "source-before.json"):
            self.assertEqual((self.out / name).read_bytes(), (destination / name).read_bytes())
        with self.assertRaises(FileExistsError):
            evidence.publish(self.out, destination, self.source)

    def test_busy_gate_cannot_start_or_replace_evidence(self):
        self.lock.write_bytes(b"original owner")
        with self.assertRaises(FileExistsError):
            self.run_attempt()
        self.assertEqual(self.lock.read_bytes(), b"original owner")
        self.assertEqual(self.calls, [])
        self.assertFalse(self.out.exists())

    def test_failed_step_stops_preserves_exit_and_gate(self):
        def fail(*args):
            self.invoke(*args)
            return gates.ExecutionResult(101)
        code, manifest = self.run_attempt(invoke=fail)
        self.assertEqual(code, 101)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(manifest["steps"][0]["exit_code"], 101)
        self.assertTrue(self.lock.exists())
        self.assertTrue((self.out / "source-after.json").exists())
        with self.assertRaises(ValueError):
            evidence.verify(self.out, self.source)

    def test_interrupt_does_not_release_or_invent_exit(self):
        def interrupt(*args):
            self.invoke(*args)
            raise KeyboardInterrupt()
        code, manifest = self.run_attempt(invoke=interrupt)
        self.assertEqual(code, 1)
        self.assertTrue(self.lock.exists())
        self.assertEqual(manifest["steps"], [])
        self.assertIn("KeyboardInterrupt", manifest["error"])
        self.assertEqual(json.loads((self.out / "01-check.started.json").read_bytes())["exit_code"], None)

    def test_candidate_mismatch_never_starts(self):
        wrong = {**self.expected, "head": "d" * 40}
        code, _ = self.run_attempt(expected=wrong)
        self.assertEqual(code, 1)
        self.assertEqual(self.calls, [])
        self.assertTrue(self.lock.exists())

    def test_dirty_input_never_starts(self):
        self.source["clean"] = False
        code, _ = self.run_attempt()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls, [])

    def test_source_bytes_changed_under_same_git_tree(self):
        def change(*args):
            result = self.invoke(*args)
            self.source["source_sha256"] = "d" * 64
            return result
        code, manifest = self.run_attempt(invoke=change)
        self.assertEqual(code, 1)
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(manifest["inputs_verified"])
        self.assertTrue(self.lock.exists())
        self.assertNotEqual((self.out / "source-before.json").read_bytes(), (self.out / "source-after.json").read_bytes())

    def test_change_at_final_snapshot_cannot_return_exit_zero(self):
        self.options["mode"] = "check"
        count = 0
        def capture(_):
            nonlocal count
            count += 1
            value = copy.deepcopy(self.source)
            if count == 4:
                value["source_sha256"] = "d" * 64
            return value
        code, manifest = self.run_attempt(capture=capture)
        self.assertEqual(code, 1)
        self.assertEqual(manifest["runner_exit_code"], 1)
        self.assertEqual(manifest["status"], "failed")
        self.assertTrue(self.lock.exists())

    def test_attempt_is_never_reused(self):
        self.run_attempt()
        raw = (self.out / "manifest.json").read_bytes()
        with self.assertRaises(FileExistsError):
            self.run_attempt()
        self.assertEqual(raw, (self.out / "manifest.json").read_bytes())

    def test_tampered_log_and_later_head_rejected(self):
        self.run_attempt()
        with self.assertRaises(ValueError):
            evidence.verify(self.out, {**self.source, "head": "e" * 40})
        (self.out / "01-check.log").write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            evidence.verify(self.out, self.source)

    def test_check_is_not_published_as_full(self):
        self.options["mode"] = "check"
        self.run_attempt()
        evidence.publish(self.out, self.root / "published", self.source)
        result = json.loads((self.root / "published/publication.json").read_bytes())
        self.assertFalse(result["full_gate_suite"])
        self.assertEqual(len(self.calls), 1)

    def test_zero_matching_tests_fails_even_with_exit_zero(self):
        self.options.update(mode="targeted", test_targets=["lifecycle_compat"], filter="missing")
        def empty(*args):
            self.invoke(*args)
            args[-1].write_bytes(b"test result: ok. 0 passed; 0 failed; 0 ignored;\n")
            return gates.ExecutionResult(0)
        code, manifest = self.run_attempt(invoke=empty)
        self.assertEqual(code, 1)
        self.assertEqual(manifest["status"], "failed")

    def test_plan_rejects_shell_and_ignored_injection(self):
        for target in ["x;shutdown", "../live", "--ignored", "a b", "$(whoami)"]:
            with self.subTest(target=target), self.assertRaises(ValueError):
                gates.plan("targeted", [target])
        with self.assertRaises(ValueError):
            gates.plan("targeted", [])
        with self.assertRaises(ValueError):
            gates.plan("targeted", ["a"], "--ignored")
        commands = gates.plan("targeted", ["lifecycle_compat"], "cancel")
        self.assertIn("--test-threads=1", commands[-1]["command"])
        self.assertNotIn("--ignored", commands[-1]["argv"])

    def test_no_automatic_gate_release_api(self):
        self.assertFalse(hasattr(gates.Gate, "release"))

    def test_lib_and_compile_only_modes_have_explicit_safe_commands(self):
        lib = gates.plan("lib", [], "process_lifetime_tests::cancel")[-1]
        self.assertEqual(lib["name"], "library-test")
        integration_lib = gates.plan("targeted", ["lib"])[-1]
        self.assertEqual(integration_lib["argv"][-3:], ["targeted", "-TestTarget", "lib"])
        self.assertIn("--lib", lib["command"])
        self.assertIn("--test-threads=1", lib["command"])
        test_build = gates.plan("test-build", ["lifecycle_compat"])[-1]
        self.assertIn("--no-run", test_build["command"])
        example = gates.plan("example-build", [], examples=["process_lifetime_probe"])[-1]
        self.assertEqual(example["command"][-2:], ["--example", "process_lifetime_probe"])
        with self.assertRaises(ValueError):
            gates.plan("lib", [], "")
        with self.assertRaises(ValueError):
            gates.plan("example-build", [], examples=["x;Invoke-Expression"])
        with self.assertRaises(ValueError):
            gates.plan("test-build", ["lifecycle_compat"], "cancel")

    def test_metadata_tampering_and_unrecorded_artifact_rejected(self):
        self.run_attempt()
        source = self.out / "source-after.json"
        source.write_bytes(source.read_bytes() + b" ")
        with self.assertRaises(ValueError):
            evidence.verify(self.out, self.source)
        (self.out / "unrecorded.log").write_bytes(b"not in manifest")
        with self.assertRaises(ValueError):
            evidence.verify(self.out, self.source)

    def test_unknown_even_with_direct_exit_zero_stops_next_step(self):
        def unknown(*args):
            self.invoke(*args)
            return gates.ExecutionResult(0, True, "child_terminal_unknown")
        code, manifest = self.run_attempt(invoke=unknown)
        self.assertEqual(code, 124)
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(manifest["steps"][0]["exit_code"], 0)
        self.assertTrue(manifest["steps"][0]["terminal_unknown"])

    def test_supervisor_retains_same_object_after_deadline_and_interrupt(self):
        from unittest.mock import Mock
        process = Mock()
        process.wait.side_effect = [KeyboardInterrupt(), gates.subprocess.TimeoutExpired("mock", 0.25), 0]
        log = self.root / "supervised.log"
        with patch.object(gates.subprocess, "Popen", return_value=process) as spawn, patch.object(gates.time, "monotonic", return_value=100):
            result = gates.execute(["mock"], self.root, {}, 1, log)
        self.assertEqual(spawn.call_count, 1)
        self.assertEqual(process.wait.call_count, 3)
        self.assertEqual(result.exit_code, 0)
        self.assertTrue(result.terminal_unknown)
        self.assertEqual(result.reason, "supervisor_interrupted")
        self.assertFalse(process.kill.called)
        self.assertFalse(process.terminate.called)

    def test_deadline_is_sticky_after_original_process_exits(self):
        from unittest.mock import Mock
        process = Mock()
        process.wait.return_value = 0
        with patch.object(gates.subprocess, "Popen", return_value=process), patch.object(gates.time, "monotonic", side_effect=[0, 2]):
            result = gates.execute(["mock"], self.root, {}, 1, self.root / "deadline.log")
        self.assertEqual(result.reason, "step_deadline_exceeded")

    def test_terminal_marker_detected_even_on_fast_exit(self):
        from unittest.mock import Mock
        def spawn(*args, **kwargs):
            kwargs["stdout"].write(b"NHC_TERMINAL_UNKNOWN fixture refused cleanup\n")
            kwargs["stdout"].flush()
            return Mock(wait=Mock(return_value=0))
        with patch.object(gates.subprocess, "Popen", side_effect=spawn):
            result = gates.execute(["mock"], self.root, {}, 10, self.root / "marker.log")
        self.assertEqual(result.reason, "child_terminal_unknown")

    def test_e_only_resolved_paths(self):
        self.assertEqual(gates.e_path(self.root, "tmp").drive.upper(), "E:")
        with self.assertRaises(ValueError):
            gates.e_path(self.root, "C:/invalid-output")

    def test_snapshot_records_dirty_and_raw_bytes(self):
        source = self.root / "NoManCode/source.rs"
        source.parent.mkdir()
        source.write_bytes(b"raw\r\n")
        def fake_git(repo, *args):
            if args[0] == "status":
                return b" M NoManCode/source.rs\0"
            if args[0] == "ls-files":
                return b"NoManCode/source.rs\0"
            return {"HEAD": self.expected["head"], "HEAD:NoManCode": self.expected["product_tree"],
                    "HEAD:" + gates.RUST_REL: self.expected["rust_tree"]}[args[-1]].encode()
        with patch.object(gates, "git", fake_git):
            result = gates.snapshot(self.root)
        self.assertFalse(result["clean"])
        self.assertEqual(result["files"][0]["sha256"], gates.digest(b"raw\r\n"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
