from __future__ import annotations

import hashlib
import importlib.util
import os
import pathlib
import subprocess
import tempfile
import unittest


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

        current_head = guard.verify_source_snapshot("git", self.root, self.snapshot)

        self.assertNotEqual(current_head, captured_head)
        self.assertEqual(current_head, self.head())

    def test_contract_source_mutation_is_refused_even_after_commit(self) -> None:
        self.contract.write_text("export FROZEN_ALIAS=/nix/store/replaced\n", encoding="utf-8")
        self.commit()

        with self.assertRaisesRegex(guard.OverlayInputError, "contract source changed: contract.nix"):
            guard.verify_source_snapshot("git", self.root, self.snapshot)

    def test_dirty_contract_source_is_refused_before_hash_acceptance(self) -> None:
        self.contract.write_text("export FROZEN_ALIAS=/nix/store/replaced\n", encoding="utf-8")

        with self.assertRaisesRegex(guard.OverlayInputError, "uncommitted changes"):
            guard.verify_source_snapshot("git", self.root, self.snapshot)

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


if __name__ == "__main__":
    unittest.main()
