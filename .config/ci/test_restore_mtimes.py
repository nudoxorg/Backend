#!/usr/bin/env python3
"""Contract tests for restore-mtimes.py; run with `python3 test_restore_mtimes.py`."""

import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "restore_mtimes", Path(__file__).with_name("restore-mtimes.py")
)
restore_mtimes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restore_mtimes)

OLD = 1_600_000_000 * 10**9


def git(root, *args):
    subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)


class RestoreMtimesTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name)
        self.manifest = str(self.root.parent / f"{self.root.name}-manifest.json")
        git(self.root, "init", "-q")
        for name in ("crate/test_laziness/test_a.md", "crate/test_laziness/test_b.md", "crate/lib.rs"):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        git(self.root, "add", ".")
        self.cwd = os.getcwd()
        os.chdir(self.root)

    def tearDown(self):
        os.chdir(self.cwd)
        self.scratch.cleanup()
        if os.path.exists(self.manifest):
            os.remove(self.manifest)

    def first_build(self):
        """Records a manifest whose sources and directories are all OLD."""
        for path in self.root.rglob("*"):
            if ".git" not in path.parts:
                os.utime(path, ns=(OLD, OLD))
        restore_mtimes.main(self.manifest)

    def fresh_clone(self):
        """A new checkout stamps every file and directory 'now'."""
        for path in self.root.rglob("*"):
            if ".git" not in path.parts:
                os.utime(path)

    def test_unchanged_directory_regains_its_contents_mtime(self):
        self.first_build()
        self.fresh_clone()
        restore_mtimes.main(self.manifest)
        self.assertEqual((self.root / "crate/test_laziness").stat().st_mtime_ns, OLD)
        self.assertEqual((self.root / "crate").stat().st_mtime_ns, OLD)

    def test_changed_file_keeps_its_directories_new(self):
        self.first_build()
        self.fresh_clone()
        (self.root / "crate/test_laziness/test_a.md").write_text("changed")
        git(self.root, "add", ".")
        restore_mtimes.main(self.manifest)
        self.assertGreater((self.root / "crate/test_laziness").stat().st_mtime_ns, OLD)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)

    def test_removed_entry_keeps_its_directory_new(self):
        self.first_build()
        self.fresh_clone()
        git(self.root, "rm", "-qf", "crate/test_laziness/test_b.md")
        restore_mtimes.main(self.manifest)
        # Remaining files are unchanged, but the listing changed: a build
        # script enumerating this directory must rerun.
        self.assertGreater((self.root / "crate/test_laziness").stat().st_mtime_ns, OLD)

    def test_manifest_without_directory_records_restores_files_only(self):
        self.first_build()
        manifest_text = Path(self.manifest).read_text()
        self.assertIn("directories", manifest_text)
        self.fresh_clone()
        Path(self.manifest).write_text(manifest_text.replace("directories", "unrelated"))
        restore_mtimes.main(self.manifest)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)
        self.assertGreater((self.root / "crate").stat().st_mtime_ns, OLD)


if __name__ == "__main__":
    unittest.main()
