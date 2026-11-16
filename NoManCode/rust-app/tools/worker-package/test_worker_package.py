"""Pure mock builds and local byte copies; never starts Cargo or product binaries."""
import copy
import json
from pathlib import Path
import tempfile
import unittest

import worker_package as package
from lifecycle_gates import ExecutionResult


class PairTests(unittest.TestCase):
    def setUp(self):
        root = package.REPO / ".local/worker-package-mocks"
        root.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=root)
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.main = self.repo / package.HOST_SOURCE
        self.main.parent.mkdir(parents=True)
        self.main.write_text('const DIGEST: Option<&str> = option_env!("NHC_PACKAGED_WORKER_SHA256");', encoding="utf-8")
        (self.repo / package.PROTOCOL_SOURCE).write_text('pub const VERSION: u32 = 1;', encoding="utf-8")
        self.expected = {"head": "a" * 40, "product_tree": "b" * 40, "rust_tree": "c" * 40}
        self.source = {**self.expected, "clean": True, "files": [{"path": package.HOST_SOURCE}], "source_sha256": "d" * 64}
        self.evidence = self.repo / "evidence/attempt"
        self.stage = self.repo / "packages/.staging-pair"
        self.pair = self.repo / "packages/pair"
        self.target = self.repo / "target"
        self.gate = self.repo / "mock-global.lock"
        self.calls = []
        old = self.repo / "packages/old"
        old.mkdir(parents=True)
        (old / "peachsh.exe").write_bytes(b"old-host")
        (old / "peachsh-wasm-worker.exe").write_bytes(b"old-worker")

    def artifact_message(self, kind, path):
        return {"reason": "compiler-artifact", "target": {
            "name": "peachsh-wasm-worker" if kind == "worker" else "peachsh", "kind": ["bin"],
            "src_path": str(self.repo / (package.WORKER_SOURCE if kind == "worker" else package.HOST_SOURCE))},
            "profile": {"test": False}, "executable": str(path), "fresh": False}

    def invoke(self, argv, cwd, env, timeout, log):
        self.assertTrue(self.gate.exists())
        kind = argv[argv.index("-Stage") + 1]
        self.calls.append((kind, env.copy(), argv.copy()))
        path = self.target / (kind + ".exe")
        payload = b"trusted-worker" if kind == "worker" else b"host-with-digest=" + env[package.DIGEST_VARIABLE].encode()
        path.write_bytes(payload)
        messages = [self.artifact_message(kind, path), {"reason": "build-finished", "success": True}]
        log.write_bytes(b"diagnostic line\n" + b"\n".join(json.dumps(message).encode() for message in messages) + b"\n")
        return ExecutionResult(0)

    def run_pair(self, **overrides):
        args = dict(repo=self.repo, evidence=self.evidence, stage=self.stage, pair=self.pair,
                    gate_path=self.gate, expected=self.expected, target_dir=self.target,
                    profile="debug", target=package.TARGET, timeout_seconds=60,
                    environment={package.DIGEST_VARIABLE: "untrusted-inherited", "NHC_PACKAGED_WORKER_PROTOCOL": "99"},
                    capture=lambda _: copy.deepcopy(self.source), invoke=self.invoke)
        args.update(overrides)
        result = package.build_pair(**args)
        self.assertEqual((self.repo / "packages/old/peachsh.exe").read_bytes(), b"old-host")
        self.assertEqual((self.repo / "packages/old/peachsh-wasm-worker.exe").read_bytes(), b"old-worker")
        return result

    def test_pair_is_frozen_before_host_build_and_old_pair_untouched(self):
        code, report = self.run_pair()
        self.assertEqual(code, 0)
        self.assertEqual(report["status"], "prepared")
        self.assertFalse(self.stage.exists())
        self.assertTrue(self.gate.exists())
        self.assertEqual([c[0] for c in self.calls], ["worker", "host"])
        self.assertNotIn(package.DIGEST_VARIABLE, self.calls[0][1])
        self.assertNotIn("NHC_PACKAGED_WORKER_PROTOCOL", self.calls[0][1])
        record = package.verify_pair(self.pair, self.expected)
        self.assertEqual(record["worker"]["sha256"], self.calls[1][1][package.DIGEST_VARIABLE])
        self.assertFalse(record["runtime_binding_verified"])
        (self.target / "worker.exe").write_bytes(b"later Cargo rebuild")
        package.verify_pair(self.pair, self.expected)

    def test_host_failure_keeps_partial_staging_but_publishes_no_pair(self):
        def fail(*args):
            result = self.invoke(*args)
            return ExecutionResult(101) if self.calls[-1][0] == "host" else result
        code, report = self.run_pair(invoke=fail)
        self.assertEqual(code, 101)
        self.assertEqual(report["status"], "failed")
        self.assertTrue((self.stage / "peachsh-wasm-worker.exe").exists())
        self.assertFalse(self.pair.exists())
        self.assertTrue(self.gate.exists())

    def test_worker_failure_does_not_start_host(self):
        def fail(*args):
            self.invoke(*args)
            return ExecutionResult(101)
        code, _ = self.run_pair(invoke=fail)
        self.assertEqual(code, 101)
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(self.pair.exists())

    def test_unknown_stops_even_when_direct_exit_is_zero(self):
        def unknown(*args):
            self.invoke(*args)
            return ExecutionResult(0, True, "step_deadline_exceeded")
        code, report = self.run_pair(invoke=unknown)
        self.assertEqual(code, 124)
        self.assertEqual(len(self.calls), 1)
        self.assertTrue(report["steps"][0]["terminal_unknown"])
        self.assertFalse(self.pair.exists())

    def test_missing_host_binding_fails_before_any_build(self):
        self.main.write_text("fn main() {}", encoding="utf-8")
        code, _ = self.run_pair()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls, [])
        self.assertFalse(self.stage.exists())
        preflight = json.loads((self.evidence / "preflight.json").read_bytes())
        self.assertEqual(preflight["status"], "failed")
        self.assertFalse(preflight["build_started"])
        self.assertIn("consumer missing", preflight["error"])

    def test_comment_or_string_is_not_a_compile_digest_consumer(self):
        for text in [
            '// option_env!("NHC_PACKAGED_WORKER_SHA256")',
            '/* outer /* inner */ option_env!("NHC_PACKAGED_WORKER_SHA256") */',
            r'let s = "option_env!(\"NHC_PACKAGED_WORKER_SHA256\")";',
            'let s = r###"option_env!("NHC_PACKAGED_WORKER_SHA256")"###;',
        ]:
            self.main.write_text(text, encoding="utf-8")
            with self.subTest(text=text), self.assertRaises(ValueError):
                package.validate_binding_source(self.repo, self.source)

    def test_library_or_worker_digest_consumer_rejected(self):
        lib = self.repo / package.RUST_REL / "src/lib.rs"
        lib.write_text('const X: &str = env!("NHC_PACKAGED_WORKER_SHA256");', encoding="utf-8")
        self.source["files"].append({"path": package.RUST_REL + "/src/lib.rs"})
        code, _ = self.run_pair()
        self.assertEqual(code, 1)
        self.assertEqual(self.calls, [])

    def test_source_drift_after_worker_does_not_start_host(self):
        def drift(*args):
            result = self.invoke(*args)
            self.source["source_sha256"] = "e" * 64
            return result
        code, _ = self.run_pair(invoke=drift)
        self.assertEqual(code, 1)
        self.assertEqual(len(self.calls), 1)
        self.assertFalse(self.pair.exists())

    def test_host_cannot_mutate_frozen_worker(self):
        def tamper(*args):
            result = self.invoke(*args)
            if self.calls[-1][0] == "host":
                (self.stage / "peachsh-wasm-worker.exe").write_bytes(b"tampered")
            return result
        code, _ = self.run_pair(invoke=tamper)
        self.assertEqual(code, 1)
        self.assertFalse(self.pair.exists())

    def test_missing_ambiguous_and_wrong_source_artifacts_fail_closed(self):
        artifact = self.target / "one.exe"
        self.target.mkdir()
        artifact.write_bytes(b"one")
        good = self.artifact_message("worker", artifact)
        other = self.artifact_message("worker", self.target / "two.exe")
        wrong = copy.deepcopy(good)
        wrong["target"]["src_path"] = str(self.repo / "dependency/main.rs")
        for messages in ([wrong], [good, other], []):
            raw = b"\n".join(json.dumps(m).encode() for m in [*messages, {"reason": "build-finished", "success": True}])
            with self.subTest(messages=messages), self.assertRaises(ValueError):
                package.select_artifact(raw, self.repo, "worker", self.target)

    def test_artifact_outside_selected_target_is_rejected(self):
        outside = self.repo / "outside.exe"
        outside.write_bytes(b"outside")
        raw = json.dumps(self.artifact_message("worker", outside)).encode()
        with self.assertRaises(ValueError):
            package.select_artifact(raw, self.repo, "worker", self.target)

    def test_existing_pair_or_gate_is_not_overwritten(self):
        self.pair.mkdir()
        with self.assertRaises(FileExistsError):
            self.run_pair()
        self.assertEqual(self.calls, [])
        self.pair.rmdir()
        self.gate.write_bytes(b"another owner")
        with self.assertRaises(FileExistsError):
            self.run_pair()
        self.assertEqual(self.gate.read_bytes(), b"another owner")

    def test_destination_race_keeps_the_existing_pair_and_partial_stage(self):
        def create_destination(*args):
            result = self.invoke(*args)
            if self.calls[-1][0] == "host":
                self.pair.mkdir()
                (self.pair / "existing").write_bytes(b"do not replace")
            return result
        code, report = self.run_pair(invoke=create_destination)
        self.assertEqual(code, 1)
        self.assertEqual(report["status"], "failed")
        self.assertEqual((self.pair / "existing").read_bytes(), b"do not replace")
        self.assertTrue((self.stage / "pair.json").exists())

    def test_output_overlap_is_rejected_before_gate(self):
        with self.assertRaises(ValueError):
            self.run_pair(evidence=self.pair)
        self.assertFalse(self.gate.exists())

    def test_protocol_is_bound_to_source_not_inherited_environment(self):
        (self.repo / package.PROTOCOL_SOURCE).write_text('pub const VERSION: u32 = 12;', encoding="utf-8")
        self.run_pair()
        record = package.verify_pair(self.pair, self.expected)
        self.assertEqual(record["protocol_version"], 12)
        self.assertNotIn("NHC_PACKAGED_WORKER_PROTOCOL", self.calls[1][1])

    def test_invalid_or_ambiguous_protocol_stops_before_build(self):
        for source in ('pub const VERSION: u32 = 0;', 'pub const VERSION: u32 = 4294967296;',
                       'pub const VERSION: u32 = 1; pub const VERSION: u32 = 2;'):
            (self.repo / package.PROTOCOL_SOURCE).write_text(source, encoding="utf-8")
            with self.subTest(source=source), self.assertRaises(ValueError):
                package.protocol_version(self.repo)

    def test_same_target_profile_and_no_install_command(self):
        self.run_pair(profile="release")  # Mock command only; does NOT run Cargo/release.
        for kind, _, argv in self.calls:
            self.assertEqual(argv[argv.index("-Profile") + 1], "release")
            self.assertEqual(argv[argv.index("-Target") + 1], package.TARGET)
            command = package.cargo_command(kind, "release", package.TARGET)
            self.assertIn("--release", command)
            self.assertIn("--locked", command)
            self.assertIn("--offline", command)
            self.assertNotIn("install", command)

    def test_pair_verification_rejects_mutated_host_or_foreign_candidate(self):
        self.run_pair()
        with self.assertRaises(ValueError):
            package.verify_pair(self.pair, {**self.expected, "head": "e" * 40})
        (self.pair / "peachsh.exe").write_bytes(b"different host")
        with self.assertRaises(ValueError):
            package.verify_pair(self.pair, self.expected)


if __name__ == "__main__":
    unittest.main(verbosity=2)
