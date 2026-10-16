"""Pure Python admission fixtures; these do not build or execute a native helper."""
from __future__ import annotations

import argparse
from contextlib import ExitStack
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location("helper_producer", Path(__file__).with_name("record_embedding_helper_build.py"))
producer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(producer)


class HelperReceiptFixtures(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        for name in ("source", "tools", "cache", "target", "build"):
            (self.root / name).mkdir()
        source = self.root / "source"
        (source / "tools/embedding-candle").mkdir(parents=True)
        (source / "tools/embedding-candle/Cargo.toml").write_text('[package]\nname="backend-embedding-candle"\n[[bin]]\nname="backend-embedding-candle"\n')
        for name in ("cargo", "rustc"):
            (self.root / "tools" / name).write_bytes(b"fixture identity only; never executed")
            (self.root / "tools" / name).chmod(0o700)
        self.args = argparse.Namespace(source_checkout=source, cargo=self.root / "tools/cargo", rustc=self.root / "tools/rustc",
                                       cargo_home=self.root / "cache", target_dir=self.root / "target", build_dir=self.root / "build",
                                       output_dir=self.root / "evidence", jobs=2, timeout_seconds=10, max_log_bytes=4096,
                                       deadline_monotonic=None, cargo_sha256=self.digest(self.root / "tools/cargo"), rustc_sha256=self.digest(self.root / "tools/rustc"))
        self.source = {"path": source, "commit": "1" * 40, "tree": "2" * 40, "cargo_lock_sha256": "3" * 64, "clean": True}
        self.artifact = self.args.target_dir / producer.TARGET / "release" / producer.NAME
        self.artifact.parent.mkdir(parents=True)
        # Structurally recognized header only. Not an actual compiled executable.
        image = bytearray(128)
        image[:6] = b"\x7fELF\x02\x01"
        image[18:20] = (62).to_bytes(2, "little")
        self.artifact.write_bytes(image)
        self.artifact.chmod(0o700)
        self.execution_calls = []

    @staticmethod
    def digest(path):
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def fixture_execution(self, command, source, env, output, deadline, maximum):
        self.execution_calls.append((list(command), source, dict(env), deadline, maximum))
        (output / "cargo.stdout").write_bytes(b"fixture Cargo output\n")
        (output / "cargo.stderr").write_bytes(b"")
        return {"exit": 0, "pid": 42, "process_group_retired": True, "output_bytes": 21}

    def fixtures(self, *, snapshots=None, execution=None):
        stack = ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(patch.object(producer.platform, "system", return_value="Linux"))
        stack.enter_context(patch.object(producer.platform, "machine", return_value="x86_64"))
        stack.enter_context(patch.object(producer, "ROOT", self.source["path"]))
        stack.enter_context(patch.object(producer.shared, "source_snapshot", side_effect=snapshots or [dict(self.source), dict(self.source)]))
        # Isolate real tool/--version execution; ordinary fixture files are NOT
        # admitted as actual Cargo or rustc by the production tool validator.
        stack.enter_context(patch.object(producer.shared, "stable_tool_file", side_effect=lambda path, *_a, **_k: producer.shared.stable_file(path, "fixture tool")))
        stack.enter_context(patch.object(producer.shared, "tool_version", return_value="fixture-version"))
        stack.enter_context(patch.object(producer, "execute", side_effect=execution or self.fixture_execution))

    def assert_no_receipt(self):
        self.assertFalse((self.args.output_dir / "embedding-helper-build-receipt.json").exists())

    def test_success_uses_exact_recipe_and_existing_consumer(self):
        self.fixtures()
        with patch.dict(os.environ, {"GIT_DIR": "/external/git", "GIT_WORK_TREE": "/external/source", "GIT_INDEX_FILE": "/external/index", "CARGO_BUILD_TARGET": "wrong-target", "RUSTC_WRAPPER": "wrong-wrapper"}):
            result = producer.capture(self.args)
        receipt = json.loads(result.read_bytes())
        command, source, env, _, maximum = self.execution_calls[0]
        self.assertEqual(command, producer.ARGV)
        self.assertEqual(source, self.source["path"])
        self.assertEqual(maximum, 4096)
        self.assertEqual(env["CARGO_BUILD_TARGET"], producer.TARGET)
        self.assertEqual(env["CARGO_NET_OFFLINE"], "true")
        self.assertEqual(env["CARGO_BUILD_JOBS"], "2")
        self.assertEqual(env["CARGO_INCREMENTAL"], "0")
        self.assertEqual(env["RUSTC_WRAPPER"], "")
        self.assertFalse({"GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"} & env.keys())
        self.assertEqual(receipt["artifact"]["sha256"], self.digest(self.artifact))
        admitted, _ = producer.package.admit_embedding_helper(self.artifact, result, self.source["commit"], self.source["tree"], self.source["cargo_lock_sha256"])
        self.assertEqual(admitted["sha256"], receipt["artifact"]["sha256"])
        self.assertFalse((self.args.output_dir / "candidate.json").exists())

    def test_successful_cached_artifact_does_not_require_new_mtime(self):
        os.utime(self.artifact, (1, 1))
        os.link(self.artifact, self.artifact.with_name(producer.NAME + ".cargo-hardlink"))
        before = self.artifact.stat()
        self.fixtures()
        producer.capture(self.args)
        self.assertEqual(self.artifact.stat().st_mtime_ns, before.st_mtime_ns)
        self.assertEqual(self.artifact.stat().st_nlink, 2)
        self.assertEqual(len(self.execution_calls), 1)

    def test_dirty_source_refuses_before_executor_or_output_creation(self):
        self.fixtures(snapshots=[{**self.source, "clean": False}])
        with self.assertRaisesRegex(ValueError, "dirty source"):
            producer.capture(self.args)
        self.assertFalse(self.execution_calls)
        self.assertFalse(self.args.output_dir.exists())

    def test_symlinked_build_or_evidence_ancestor_refuses_before_execution(self):
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        for field, path in (("target_dir", alias / "target"), ("output_dir", alias / "tools/evidence")):
            with self.subTest(field=field):
                original = getattr(self.args, field)
                setattr(self.args, field, path)
                self.fixtures()
                with self.assertRaisesRegex(ValueError, "canonical path"):
                    producer.capture(self.args)
                self.assertFalse(self.execution_calls)
                self.assert_no_receipt()
                setattr(self.args, field, original)

    def test_source_drift_cannot_publish(self):
        for key, value in (("commit", "4" * 40), ("tree", "5" * 40), ("cargo_lock_sha256", "6" * 64), ("clean", False)):
            with self.subTest(field=key):
                self.args.output_dir = self.root / ("evidence-" + key)
                self.fixtures(snapshots=[dict(self.source), {**self.source, key: value}])
                with self.assertRaisesRegex(ValueError, "source changed"):
                    producer.capture(self.args)
                self.assert_no_receipt()

    def test_failed_command_and_unretired_group_cannot_publish(self):
        for field, value in (("exit", 101), ("process_group_retired", False)):
            with self.subTest(field=field):
                self.args.output_dir = self.root / ("evidence-" + field)

                def failed(*args):
                    return {**self.fixture_execution(*args), field: value}

                self.fixtures(execution=failed)
                with self.assertRaisesRegex(ValueError, "did not succeed"):
                    producer.capture(self.args)
                self.assert_no_receipt()
                self.assertTrue((self.args.output_dir / "FAILED.json").exists())

    def test_tool_change_after_success_cannot_publish(self):
        def changed(*args):
            result = self.fixture_execution(*args)
            self.args.rustc.write_bytes(b"different fixture identity")
            return result

        self.fixtures(execution=changed)
        with self.assertRaisesRegex(ValueError, "admitted SHA256"):
            producer.capture(self.args)
        self.assert_no_receipt()

    def test_missing_corrupt_or_symlink_artifact_cannot_publish(self):
        for kind in ("missing", "corrupt", "symlink"):
            with self.subTest(kind=kind):
                self.args.output_dir = self.root / ("evidence-" + kind)
                if self.artifact.exists() or self.artifact.is_symlink():
                    self.artifact.unlink()
                if kind == "corrupt":
                    self.artifact.write_bytes(b"not an ELF")
                    self.artifact.chmod(0o700)
                elif kind == "symlink":
                    self.artifact.symlink_to(self.args.cargo)
                self.fixtures()
                with self.assertRaises((ValueError, OSError)):
                    producer.capture(self.args)
                self.assert_no_receipt()
                self.assertTrue((self.args.output_dir / "FAILED.json").exists())
        self.assertEqual(len(self.execution_calls), 3)

    def test_artifact_change_after_consumer_admission_cannot_publish(self):
        self.fixtures()
        consumer = producer.package.admit_embedding_helper

        def admitted_then_changed(*args):
            result = consumer(*args)
            with self.artifact.open("ab") as stream:
                stream.write(b"changed fixture bytes")
            return result

        with patch.object(producer.package, "admit_embedding_helper", side_effect=admitted_then_changed):
            with self.assertRaisesRegex(ValueError, "changed before receipt publication"):
                producer.capture(self.args)
        self.assert_no_receipt()

    def test_final_directory_sync_failure_cannot_report_or_leave_receipt(self):
        self.fixtures()
        with patch.object(producer, "sync_directory", side_effect=OSError("fixture directory sync failure")) as sync:
            with self.assertRaisesRegex(OSError, "directory sync failure"):
                producer.capture(self.args)
        sync.assert_called_once_with(self.args.output_dir)
        self.assert_no_receipt()
        self.assertTrue((self.args.output_dir / "FAILED.json").exists())

    def test_consumer_rejection_and_expired_proof_cannot_publish(self):
        self.fixtures()
        with patch.object(producer.package, "admit_embedding_helper", side_effect=ValueError("fixture consumer refusal")):
            with self.assertRaisesRegex(ValueError, "consumer refusal"):
                producer.capture(self.args)
        self.assert_no_receipt()
        self.args.output_dir = self.root / "expired"
        self.args.deadline_monotonic = time.monotonic() - 1
        with self.assertRaisesRegex(ValueError, "expired"):
            producer.capture(self.args)
        self.assert_no_receipt()
        for invalid in (float("nan"), float("inf")):
            self.args.deadline_monotonic = invalid
            with self.assertRaisesRegex(ValueError, "finite and positive"):
                producer.capture(self.args)
            self.assert_no_receipt()
        self.assertEqual(len(self.execution_calls), 1)


class ActualPythonPipeBounds(unittest.TestCase):
    """Exercise only CPython pipe supervision, never Cargo or a helper."""
    def test_bounded_stdout_stderr_and_nonzero_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            result = producer.execute([sys.executable, "-I", "-S", "-c", "import sys; print('out'); print('err', file=sys.stderr); sys.exit(7)"], output, dict(os.environ), output, time.monotonic() + 10, 1024)
            self.assertEqual(result["exit"], 7)
            self.assertTrue(result["process_group_retired"])
            self.assertEqual((output / "cargo.stdout").read_bytes(), b"out\n")
            self.assertEqual((output / "cargo.stderr").read_bytes(), b"err\n")

    def test_output_overflow_is_retained_only_to_exact_bound(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            with self.assertRaisesRegex(ValueError, "combined output bound"):
                producer.execute([sys.executable, "-I", "-S", "-c", "import os; os.write(1, b'x' * 100000)"], output, dict(os.environ), output, time.monotonic() + 10, 4096)
            self.assertEqual((output / "cargo.stdout").stat().st_size + (output / "cargo.stderr").stat().st_size, 4096)

    def test_silent_child_is_stopped_at_finite_deadline(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary)
            with self.assertRaisesRegex(ValueError, "finite deadline"):
                producer.execute([sys.executable, "-I", "-S", "-c", "import time; time.sleep(30)"], output, dict(os.environ), output, time.monotonic() + 0.1, 1024)


if __name__ == "__main__":
    unittest.main()
