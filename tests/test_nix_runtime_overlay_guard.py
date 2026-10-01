from __future__ import annotations

import hashlib
import importlib.util
import os
import pathlib
import queue
import subprocess
import threading
import tempfile
import time
import unittest
from unittest import mock


SCRIPT = pathlib.Path(__file__).parents[1] / ".config/scripts/nix_runtime_overlay_guard.py"
SPEC = importlib.util.spec_from_file_location("nix_runtime_overlay_guard", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
guard = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard)


class OverlaySourceGuardTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="nix-overlay-guard-")
        self.root = pathlib.Path(self.temporary.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        subprocess.run(["git", "-C", str(self.root), "config", "user.name", "Overlay Test"], check=True)
        subprocess.run(
            ["git", "-C", str(self.root), "config", "user.email", "overlay-test@example.invalid"],
            check=True,
        )
        self.contract = self.root / "contract.nix"
        self.contract.write_text("export FROZEN_ALIAS=/nix/store/tool\n", encoding="utf-8")
        (self.root / "README.md").write_text("baseline\n", encoding="utf-8")
        self.commit()
        self.snapshot = {
            "git_head": self.head(),
            "files_sha256": {
                "contract.nix": hashlib.sha256(self.contract.read_bytes()).hexdigest(),
            },
        }

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def git(self, *arguments: str) -> str:
        return subprocess.run(
            ["git", "-C", str(self.root), *arguments],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def head(self) -> str:
        return self.git("rev-parse", "HEAD")

    def commit(self) -> None:
        self.git("add", "--all")
        self.git("commit", "-q", "-m", "source snapshot")

    def test_unrelated_source_commit_preserves_contract_snapshot(self) -> None:
        captured_head = self.snapshot["git_head"]
        (self.root / "README.md").write_text("unrelated documentation change\n", encoding="utf-8")
        self.commit()

        current_head = guard.verify_source_snapshot(
            "git", self.root, self.snapshot, frozenset({"contract.nix"})
        )

        self.assertNotEqual(current_head, captured_head)
        self.assertEqual(current_head, self.head())

    def test_contract_source_mutation_is_refused_even_after_commit(self) -> None:
        self.contract.write_text("export FROZEN_ALIAS=/nix/store/replaced\n", encoding="utf-8")
        self.commit()

        with self.assertRaisesRegex(guard.OverlayInputError, "contract source changed: contract.nix"):
            guard.verify_source_snapshot(
                "git", self.root, self.snapshot, frozenset({"contract.nix"})
            )

    def test_dirty_contract_source_is_refused_before_hash_acceptance(self) -> None:
        self.contract.write_text("export FROZEN_ALIAS=/nix/store/replaced\n", encoding="utf-8")

        with self.assertRaisesRegex(guard.OverlayInputError, "uncommitted changes"):
            guard.verify_source_snapshot(
                "git", self.root, self.snapshot, frozenset({"contract.nix"})
            )

    def test_manifest_must_match_a_nonempty_closed_source_allowlist(self) -> None:
        empty = {**self.snapshot, "files_sha256": {}}
        with self.assertRaisesRegex(guard.OverlayInputError, "no contract-file hashes"):
            guard.verify_source_snapshot("git", self.root, empty, frozenset({"contract.nix"}))

        extra = {
            **self.snapshot,
            "files_sha256": {
                **self.snapshot["files_sha256"],
                "README.md": hashlib.sha256((self.root / "README.md").read_bytes()).hexdigest(),
            },
        }
        with self.assertRaisesRegex(guard.OverlayInputError, "source snapshot input set changed"):
            guard.verify_source_snapshot("git", self.root, extra, frozenset({"contract.nix"}))

    def test_runtime_provider_replacement_is_refused(self) -> None:
        provider = self.root / "tool"
        provider.write_bytes(b"pinned executable bytes")
        provider.chmod(0o755)
        resolved = provider.resolve(strict=True)
        identity = {
            "path": str(provider),
            "resolved_path": str(resolved),
            "sha256": hashlib.sha256(provider.read_bytes()).hexdigest(),
        }
        guard.verify_provider_identity("fixture-tool", identity)

        provider.write_bytes(b"replacement executable bytes")

        with self.assertRaisesRegex(guard.OverlayInputError, "runtime provider content changed: fixture-tool"):
            guard.verify_provider_identity("fixture-tool", identity)

    def test_file_mutation_during_chunked_hash_is_refused(self) -> None:
        provider = self.root / "large-provider"
        provider.write_bytes(b"a" * (2 * 1024 * 1024))
        first_chunk_read = threading.Event()
        resume_reader = threading.Event()
        result: queue.Queue[BaseException | str] = queue.Queue()
        real_read = os.read
        paused = False

        def read_with_barrier(descriptor: int, size: int) -> bytes:
            nonlocal paused
            block = real_read(descriptor, size)
            if block and not paused:
                paused = True
                first_chunk_read.set()
                if not resume_reader.wait(timeout=5):
                    raise TimeoutError("test did not release the hash reader")
            return block

        def hash_provider() -> None:
            try:
                result.put(guard.sha256_file(provider))
            except BaseException as error:  # transfer the worker failure to the test thread
                result.put(error)

        worker = threading.Thread(target=hash_provider, name="overlay-hash-test", daemon=True)
        with mock.patch.object(guard.os, "read", side_effect=read_with_barrier):
            worker.start()
            self.assertTrue(first_chunk_read.wait(timeout=5), "hash reader should reach its first chunk")
            with provider.open("r+b") as output:
                output.seek(0, os.SEEK_END)
                output.write(b"!")
                output.flush()
            resume_reader.set()
            worker.join(timeout=5)

        self.assertFalse(worker.is_alive(), "hash reader should finish after it is released")
        error = result.get_nowait()
        self.assertIsInstance(error, guard.OverlayInputError)
        self.assertIn("changed while it was being hashed", str(error))

    def test_same_path_rename_during_chunked_hash_is_refused(self) -> None:
        provider = self.root / "provider-link"
        target = self.root / "large-provider"
        replacement = self.root / "replacement-provider"
        target.write_bytes(b"b" * (2 * 1024 * 1024))
        replacement.write_bytes(b"replacement bytes")
        provider.symlink_to(target)
        identity = {
            "path": str(provider),
            "resolved_path": str(target.resolve(strict=True)),
            "sha256": hashlib.sha256(target.read_bytes()).hexdigest(),
        }
        first_chunk_read = threading.Event()
        resume_reader = threading.Event()
        result: queue.Queue[BaseException | str] = queue.Queue()
        real_read = os.read
        paused = False

        def read_with_barrier(descriptor: int, size: int) -> bytes:
            nonlocal paused
            block = real_read(descriptor, size)
            if block and not paused:
                paused = True
                first_chunk_read.set()
                if not resume_reader.wait(timeout=5):
                    raise TimeoutError("test did not release the hash reader")
            return block

        def hash_provider() -> None:
            try:
                result.put(guard.verify_provider_identity("fixture-tool", identity))
            except BaseException as error:  # transfer the worker failure to the test thread
                result.put(error)

        worker = threading.Thread(target=hash_provider, name="overlay-hash-test", daemon=True)
        with mock.patch.object(guard.os, "read", side_effect=read_with_barrier):
            worker.start()
            self.assertTrue(first_chunk_read.wait(timeout=5), "hash reader should reach its first chunk")
            provider.unlink()
            provider.symlink_to(replacement)
            resume_reader.set()
            worker.join(timeout=5)

        self.assertFalse(worker.is_alive(), "hash reader should finish after it is released")
        error = result.get_nowait()
        self.assertIsInstance(error, guard.OverlayInputError)
        self.assertIn("runtime provider path changed while hashing", str(error))

    @unittest.skipUnless(hasattr(os, "mkfifo"), "named pipes are unavailable on this platform")
    def test_special_file_is_rejected_without_blocking(self) -> None:
        pipe = self.root / "provider-pipe"
        os.mkfifo(pipe)
        started = time.monotonic()

        with self.assertRaisesRegex(guard.OverlayInputError, "not a regular file"):
            guard.sha256_file(pipe)

        self.assertLess(time.monotonic() - started, 1.0)

    def test_git_output_capture_has_a_hard_bound(self) -> None:
        fake_git = self.root / "fake-git-output"
        fake_git.write_text("#!/bin/sh\nprintf '%20000s' x\n", encoding="utf-8")
        fake_git.chmod(0o755)

        with self.assertRaisesRegex(guard.OverlayInputError, "exceeded its output limit"):
            guard._git(str(fake_git), self.root, "status")

    def test_git_command_has_a_deadline_and_stops_its_child(self) -> None:
        fake_git = self.root / "fake-git-hang"
        fake_git.write_text("#!/bin/sh\nwhile :; do :; done\n", encoding="utf-8")
        fake_git.chmod(0o755)

        with mock.patch.object(guard, "GIT_COMMAND_TIMEOUT_SECONDS", 0.05):
            started = time.monotonic()
            with self.assertRaisesRegex(guard.OverlayInputError, "exceeded its time limit"):
                guard._git(str(fake_git), self.root, "status")

        self.assertLess(time.monotonic() - started, 2.0)


if __name__ == "__main__":
    unittest.main()
