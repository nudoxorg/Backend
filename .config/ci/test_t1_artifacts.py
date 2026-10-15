"""Artifact consumers must reject wrong identities and damaged/unsafe payloads."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import t1_artifacts

from t1_artifacts import BINARIES, TARGET, digest, verify

SHA = "a" * 40
STORE = "/nix/store/" + "b" * 32 + "-runtime"


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "bin").mkdir()
        names = [*(f"bin/{name}" for name in BINARIES), "smoke.py", "runtime.nar"]
        for name in names:
            (self.root / name).write_bytes(b"fixture")
        self.manifest = {
            "schema": 1, "source_sha": SHA, "target": TARGET, "toolchain": "fixture",
            "runtime_roots": [STORE], "python": STORE + "/bin/python3",
            "runtime_env": {"PATH": STORE + "/bin"},
            "files": [{"path": name, "bytes": 7, "sha256": digest(self.root / name)} for name in names],
        }
        self.save()

    def save(self):
        (self.root / "manifest.json").write_text(json.dumps(self.manifest))

    def reject(self):
        with self.assertRaises(ValueError):
            verify(self.root, SHA, TARGET)

    def test_complete_artifact(self):
        self.assertEqual(verify(self.root, SHA, TARGET)["source_sha"], SHA)

    def test_retained_inventory_binds_the_actual_payload_without_environment(self):
        with patch.dict(os.environ, {"FORGEJO_TOKEN": "private-token"}):
            record = t1_artifacts.inventory(self.root, SHA)
        self.assertEqual(record["artifact_manifest_sha256"], digest(self.root / "manifest.json"))
        self.assertEqual(record["files"], self.manifest["files"])
        self.assertEqual(record["source_sha"], SHA)
        self.assertNotIn("runtime_env", record)
        self.assertNotIn("private-token", json.dumps(record))
        (self.root / "bin/backend-cli").write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            t1_artifacts.inventory(self.root, SHA)

    def test_inventory_toolchain_preview_is_bounded_and_its_full_identity_retained(self):
        import hashlib
        self.manifest["toolchain"] = "\x00" * 20000
        self.save()
        record = t1_artifacts.inventory(self.root, SHA)
        self.assertLess(len(json.dumps(record).encode("utf-8")), 16384)
        self.assertTrue(record["toolchain_truncated"])
        self.assertEqual(record["toolchain_sha256"], hashlib.sha256(self.manifest["toolchain"].encode()).hexdigest())
        self.assertEqual(record["artifact_manifest_sha256"], digest(self.root / "manifest.json"))

    def test_inventory_cli_is_read_only_and_does_not_import_or_launch(self):
        import contextlib
        import io
        output = io.StringIO()
        with patch("sys.argv", ["t1_artifacts.py", "inventory", str(self.root), "--source-sha", SHA]), patch.object(t1_artifacts.subprocess, "run") as run, contextlib.redirect_stdout(output):
            t1_artifacts.main()
        self.assertEqual(json.loads(output.getvalue())["artifact_manifest_sha256"], digest(self.root / "manifest.json"))
        run.assert_not_called()

    def test_exact_source_and_target_required(self):
        for source, target in [("c" * 40, TARGET), (SHA, "aarch64-unknown-linux-gnu"), ("a" * 8, TARGET)]:
            with self.assertRaises(ValueError):
                verify(self.root, source, target)

    def test_missing_or_invalid_toolchain_identity_rejected(self):
        for identity in [None, "", "   ", 42]:
            self.manifest["toolchain"] = identity
            self.save()
            self.reject()
        del self.manifest["toolchain"]
        self.save()
        self.reject()

    def test_schema_mismatch(self):
        self.manifest["schema"] = 2
        self.save()
        self.reject()

    def test_changed_binary_or_runtime_rejected(self):
        for name in ["bin/backend-cli", "runtime.nar"]:
            path = self.root / name
            path.write_bytes(b"changed")
            self.reject()
            path.write_bytes(b"fixture")

    def test_missing_binary_rejected(self):
        (self.root / "bin/backend-desktop").unlink()
        self.reject()

    def test_duplicate_or_traversal_inventory_rejected(self):
        for name in [self.manifest["files"][1]["path"], "../outside"]:
            self.manifest["files"][0]["path"] = name
            self.save()
            self.reject()

    def test_symlinked_binary_rejected(self):
        path = self.root / "bin/backend-cli"
        path.unlink()
        path.symlink_to(self.root / "smoke.py")
        self.reject()

    def test_symlinked_inventory_rejected(self):
        path = self.root / "manifest.json"
        original = path.read_text()
        path.unlink()
        (self.root / "other.json").write_text(original)
        path.symlink_to(self.root / "other.json")
        self.reject()

    def test_secret_or_mutable_environment_rejected(self):
        for env in [{"FORGEJO_TOKEN": "secret"}, {"PATH": "/usr/bin"}, {"PATH": STORE + "/bin:/tmp/bin"}]:
            self.manifest["runtime_env"] = env
            self.save()
            self.reject()

    def test_unexported_runtime_root_rejected(self):
        self.manifest["python"] = "/nix/store/" + "c" * 32 + "-other/bin/python3"
        self.save()
        self.reject()

    def test_relative_consumer_paths_survive_private_smoke_cwd(self):
        relative = os.path.relpath(self.root)
        with patch("sys.argv", ["t1_artifacts.py", "verify", relative, "--source-sha", SHA, "--smoke"]), patch.object(t1_artifacts.subprocess, "run") as run:
            t1_artifacts.main()
        self.assertEqual(run.call_count, 4)
        for call in run.call_args_list[1:]:
            argv = call.args[0]
            self.assertTrue(Path(argv[1]).is_absolute())
            self.assertTrue(Path(argv[-1]).is_absolute())

    def test_smoke_state_is_private_on_consumer_volume_and_cleaned_after_failure(self):
        import subprocess
        scratch_paths = []
        def consume(argv, **kwargs):
            if argv[0] == "nix-store":
                return
            env = kwargs["env"]
            scratch = Path(env["TMPDIR"])
            scratch_paths.append(scratch)
            self.assertEqual(scratch.parent, Path.cwd())
            self.assertEqual(scratch.stat().st_mode & 0o777, 0o700)
            self.assertNotIn("FORGEJO_TOKEN", env)
            self.assertNotIn(str(self.root), str(scratch))
            self.assertEqual(kwargs["timeout"], 180)
            raise subprocess.CalledProcessError(1, argv)
        with patch("sys.argv", ["t1_artifacts.py", "verify", str(self.root), "--source-sha", SHA, "--smoke"]), patch.object(t1_artifacts.subprocess, "run", side_effect=consume), patch.dict(os.environ, {"FORGEJO_TOKEN": "private"}):
            with self.assertRaises(subprocess.CalledProcessError):
                t1_artifacts.main()
        self.assertEqual(len(scratch_paths), 1)
        self.assertFalse(scratch_paths[0].exists())

    def test_runtime_only_consumer_verifies_then_imports_without_smoke(self):
        with patch("sys.argv", ["t1_artifacts.py", "verify", str(self.root), "--source-sha", SHA, "--import-runtime"]), patch.object(t1_artifacts.subprocess, "run") as run:
            t1_artifacts.main()
        self.assertEqual(run.call_count, 1)
        self.assertEqual(run.call_args.args[0], ["nix-store", "--import"])

    def test_damaged_runtime_is_never_imported(self):
        (self.root / "runtime.nar").write_bytes(b"damaged")
        with patch("sys.argv", ["t1_artifacts.py", "verify", str(self.root), "--source-sha", SHA, "--import-runtime"]), patch.object(t1_artifacts.subprocess, "run") as run:
            with self.assertRaises(ValueError):
                t1_artifacts.main()
        run.assert_not_called()

    def test_invalid_runtime_root_rejected(self):
        self.manifest["runtime_roots"] = ["/tmp/runtime"]
        self.save()
        self.reject()


class ProducerTests(unittest.TestCase):
    def test_publish_exports_runtime_and_excludes_credentials(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory)
            binaries = base / "input"
            binaries.mkdir()
            for name in BINARIES:
                (binaries / name).write_bytes(b"product")
            output = base / "output"
            calls = []

            def command(*args):
                calls.append(args)
                if args[:3] == ("git", "rev-parse", "HEAD"):
                    return SHA
                if args[:3] == ("git", "status", "--porcelain"):
                    return ""
                if args == ("rustc", "-vV"):
                    return "rustc fixture\nhost: " + TARGET
                if args[0] == "ldd":
                    return "libfixture => " + STORE + "/lib/libfixture.so"
                if args[:3] == ("nix-store", "--query", "--requisites"):
                    return STORE
                if args[:3] == ("nix-store", "--query", "--size"):
                    return "123"
                self.fail(args)

            def export(args, **kwargs):
                self.assertEqual(args, ["nix-store", "--export", STORE])
                kwargs["stdout"].write(b"nar fixture")

            with patch.object(t1_artifacts, "command", side_effect=command), patch.object(t1_artifacts.shutil, "which", side_effect=lambda name: STORE + "/bin/" + name), patch.object(t1_artifacts.subprocess, "run", side_effect=export), patch.dict(t1_artifacts.os.environ, {"FORGEJO_TOKEN": "private", "LD_LIBRARY_PATH": STORE + "/lib"}, clear=True):
                t1_artifacts.publish(binaries, output)
            result = verify(output, SHA, TARGET)
            self.assertNotIn("FORGEJO_TOKEN", result["runtime_env"])
            self.assertTrue((output / "runtime.nar").exists())
            self.assertEqual((output / "bin/backend-cli").read_bytes(), b"product")
            self.assertFalse(any(call[0] == "cargo" for call in calls))

    def test_dirty_sources_refuse_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            with patch.object(t1_artifacts, "command", side_effect=[SHA, " M source.rs"]):
                with self.assertRaisesRegex(ValueError, "clean tracked checkout"):
                    t1_artifacts.publish(Path(directory), output)
            self.assertFalse(output.exists())

    def test_foreign_host_refuses_publication(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "output"
            with patch.object(t1_artifacts, "command", side_effect=[SHA, "", "host: aarch64-unknown-linux-gnu"]):
                with self.assertRaisesRegex(ValueError, "currently supports"):
                    t1_artifacts.publish(Path(directory), output)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
