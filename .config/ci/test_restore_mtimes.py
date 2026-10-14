#!/usr/bin/env python3
"""Contract tests for restore-mtimes.py; run with `python3 test_restore_mtimes.py`."""

import importlib.util
import json
import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "restore_mtimes", Path(__file__).with_name("restore-mtimes.py")
)
restore_mtimes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(restore_mtimes)

OLD = 1_600_000_000 * 10**9


def git(root, *args):
    subprocess.run(["git", *args], cwd=root, check=True, capture_output=True)


def commit(root, message="fixture update"):
    git(
        root,
        "-c",
        "user.name=Restore Mtimes Test",
        "-c",
        "user.email=restore-mtimes@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-qm",
        message,
    )


class RestoreMtimesTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name)
        self.manifest = str(self.root.parent / f"{self.root.name}-manifest.json")
        self.external_targets = []
        git(self.root, "init", "-q")
        for name in ("crate/test_laziness/test_a.md", "crate/test_laziness/test_b.md", "crate/lib.rs"):
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(name)
        git(self.root, "add", ".")
        commit(self.root, "initial fixture")
        self.cwd = os.getcwd()
        os.chdir(self.root)

    def tearDown(self):
        os.chdir(self.cwd)
        for target in self.external_targets:
            target.unlink(missing_ok=True)
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

    def build_marker(self):
        """Returns an output timestamp from a simulated completed build."""
        marker = self.root.parent / f"{self.root.name}-build-marker"
        if marker not in self.external_targets:
            self.external_targets.append(marker)
        marker.write_text("build completed")
        # A completed build's output must be newer than every input. Model
        # that order explicitly instead of depending on filesystem clock ticks.
        inputs = [path.stat().st_mtime_ns for path in self.root.rglob("*")
                  if ".git" not in path.parts and not path.is_symlink()]
        timestamp = max([time.time_ns(), *inputs]) + 1_000_000
        os.utime(marker, ns=(timestamp, timestamp))
        return marker.stat().st_mtime_ns

    def changed_after(self, directory, output_time):
        """Model a later input mutation even on a coarsely timestamped FS."""
        timestamp = max(time.time_ns(), output_time) + 1_000_000
        while True:
            os.utime(directory, ns=(timestamp, timestamp))
            if directory == self.root:
                break
            directory = directory.parent

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
        commit(self.root)
        restore_mtimes.main(self.manifest)
        self.assertGreater((self.root / "crate/test_laziness").stat().st_mtime_ns, OLD)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)

    def test_removed_entry_keeps_its_directory_new(self):
        self.first_build()
        self.fresh_clone()
        git(self.root, "rm", "-qf", "crate/test_laziness/test_b.md")
        commit(self.root)
        restore_mtimes.main(self.manifest)
        # Remaining files are unchanged, but the listing changed: a build
        # script enumerating this directory must rerun.
        self.assertGreater((self.root / "crate/test_laziness").stat().st_mtime_ns, OLD)

    def test_untracked_input_keeps_watched_directory_and_unchanged_file_times(self):
        self.first_build()
        self.fresh_clone()
        watched = self.root / "crate/test_laziness"
        (watched / "new_input.md").write_text("untracked build input")
        self.assertGreater(watched.stat().st_mtime_ns, OLD)

        restore_mtimes.main(self.manifest)

        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertGreater((self.root / "crate").stat().st_mtime_ns, OLD)
        signatures = json.loads(Path(self.manifest).read_text())[restore_mtimes.DIRECTORIES]
        self.assertNotIn("crate/test_laziness", signatures)
        self.assertNotIn("crate", signatures)
        for name in (
            "crate/test_laziness/test_a.md",
            "crate/test_laziness/test_b.md",
            "crate/lib.rs",
        ):
            self.assertEqual((self.root / name).stat().st_mtime_ns, OLD)

        build_mtime = self.build_marker()
        (watched / "new_input.md").unlink()
        self.changed_after(watched, build_mtime)
        restore_mtimes.main(self.manifest)
        self.assertGreater(watched.stat().st_mtime_ns, build_mtime)
        self.assertGreater((self.root / "crate").stat().st_mtime_ns, build_mtime)
        signatures = json.loads(Path(self.manifest).read_text())[restore_mtimes.DIRECTORIES]
        self.assertIn("crate/test_laziness", signatures)

        next_build_mtime = self.build_marker()
        restore_mtimes.main(self.manifest)
        self.assertLess(watched.stat().st_mtime_ns, next_build_mtime)
        self.assertLess((self.root / "crate").stat().st_mtime_ns, next_build_mtime)

    def assert_removed_input_survives_interrupted_build(self, ignored=False, tracked=False):
        if ignored:
            (self.root / '.gitignore').write_text('crate/test_laziness/input.md\n')
            git(self.root, 'add', '.gitignore')
            commit(self.root, 'ignore directory input')
        self.first_build()
        self.fresh_clone()
        watched = self.root / 'crate/test_laziness'
        if tracked:
            removed = watched / 'test_b.md'
        else:
            removed = watched / 'input.md'
            removed.write_text('input consumed by the previous build')
        restore_mtimes.main(self.manifest)
        cached_output = self.build_marker()
        if tracked:
            git(self.root, 'rm', '-qf', str(removed.relative_to(self.root)))
            commit(self.root, 'remove tracked directory input')
        else:
            removed.unlink()
        self.changed_after(watched, cached_output)
        restore_mtimes.main(self.manifest)
        self.assertGreater(watched.stat().st_mtime_ns, cached_output)
        # No build completed. A fresh checkout/retry must not put this
        # directory behind the still-cached output from before the removal.
        self.fresh_clone()
        restore_mtimes.main(self.manifest)
        self.assertGreater(watched.stat().st_mtime_ns, cached_output)
        self.assertGreater((self.root / 'crate').stat().st_mtime_ns, cached_output)
        completed_output = self.build_marker()
        self.fresh_clone()
        restore_mtimes.main(self.manifest)
        self.assertLess(watched.stat().st_mtime_ns, completed_output)
        self.assertLess((self.root / 'crate').stat().st_mtime_ns, completed_output)

    def test_legacy_directory_signature_migration_preserves_retry_invalidation(self):
        self.first_build()
        cached_output = self.build_marker()
        manifest = json.loads(Path(self.manifest).read_text())
        manifest[restore_mtimes.DIRECTORIES] = {
            key: value['digest'] if isinstance(value, dict) else value
            for key, value in manifest[restore_mtimes.DIRECTORIES].items()
        }
        Path(self.manifest).write_text(json.dumps(manifest))
        watched = self.root/'crate/test_laziness'
        self.fresh_clone()
        self.changed_after(watched, cached_output)
        restore_mtimes.main(self.manifest)
        self.assertGreater(watched.stat().st_mtime_ns, cached_output)
        self.fresh_clone()
        restore_mtimes.main(self.manifest)
        self.assertGreater(watched.stat().st_mtime_ns, cached_output)
        completed_output = self.build_marker()
        self.fresh_clone()
        restore_mtimes.main(self.manifest)
        self.assertLess(watched.stat().st_mtime_ns, completed_output)

    def test_untracked_removal_stays_invalidated_after_interrupted_build(self):
        self.assert_removed_input_survives_interrupted_build()

    def test_ignored_removal_stays_invalidated_after_interrupted_build(self):
        self.assert_removed_input_survives_interrupted_build(ignored=True)

    def test_tracked_removal_stays_invalidated_after_interrupted_build(self):
        self.assert_removed_input_survives_interrupted_build(tracked=True)

    def test_ignored_input_keeps_watched_directory_new(self):
        (self.root / ".gitignore").write_text("crate/test_laziness/ignored.md\n")
        git(self.root, "add", ".gitignore")
        commit(self.root, "ignore fixture input")
        self.first_build()
        self.fresh_clone()
        watched = self.root / "crate/test_laziness"
        ignored = watched / "ignored.md"
        ignored.write_text("ignored build input")
        self.assertEqual(
            subprocess.run(
                ["git", "check-ignore", "-q", str(ignored.relative_to(self.root))],
                cwd=self.root,
            ).returncode,
            0,
        )
        self.assertGreater(watched.stat().st_mtime_ns, OLD)

        restore_mtimes.main(self.manifest)

        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertGreater((self.root / "crate").stat().st_mtime_ns, OLD)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)
        signatures = json.loads(Path(self.manifest).read_text())[restore_mtimes.DIRECTORIES]
        self.assertNotIn("crate/test_laziness", signatures)

        build_mtime = self.build_marker()
        ignored.unlink()
        self.changed_after(watched, build_mtime)
        restore_mtimes.main(self.manifest)
        self.assertGreater(watched.stat().st_mtime_ns, build_mtime)
        signatures = json.loads(Path(self.manifest).read_text())[restore_mtimes.DIRECTORIES]
        self.assertIn("crate/test_laziness", signatures)

        next_build_mtime = self.build_marker()
        restore_mtimes.main(self.manifest)
        self.assertLess(watched.stat().st_mtime_ns, next_build_mtime)

    def test_untracked_symlink_keeps_watched_directory_new(self):
        self.first_build()
        self.fresh_clone()
        target = self.root.parent / f"{self.root.name}-symlink-target"
        target.write_text("outside checkout")
        self.external_targets.append(target)
        target_mtime = target.stat().st_mtime_ns
        watched = self.root / "crate/test_laziness"
        link = watched / "input_link"
        link.symlink_to(target)
        self.assertGreater(watched.stat().st_mtime_ns, OLD)

        restore_mtimes.main(self.manifest)

        self.assertTrue(link.is_symlink())
        self.assertEqual(target.stat().st_mtime_ns, target_mtime)
        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)
        target.unlink(missing_ok=True)

    def test_symlink_replacing_tracked_file_is_not_followed(self):
        self.first_build()
        self.fresh_clone()
        target = self.root.parent / f"{self.root.name}-replacement-target"
        target.write_text("outside checkout")
        self.external_targets.append(target)
        target_mtime = target.stat().st_mtime_ns
        watched = self.root / "crate/test_laziness"
        tracked = watched / "test_a.md"
        tracked.unlink()
        tracked.symlink_to(target)
        git(self.root, "add", "crate/test_laziness/test_a.md")
        commit(self.root, "replace tracked file with symlink")
        self.assertGreater(watched.stat().st_mtime_ns, OLD)

        restore_mtimes.main(self.manifest)

        self.assertTrue(tracked.is_symlink())
        self.assertEqual(target.stat().st_mtime_ns, target_mtime)
        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertEqual((watched / "test_b.md").stat().st_mtime_ns, OLD)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)
        target.unlink(missing_ok=True)

    def test_tracked_symlink_target_change_keeps_watched_directory_new(self):
        self.first_build()
        watched = self.root / "crate/test_laziness"
        link = watched / "input_link"
        link.symlink_to("test_a.md")
        git(self.root, "add", "crate/test_laziness/input_link")
        commit(self.root, "add tracked input symlink")
        restore_mtimes.main(self.manifest)

        link.unlink()
        link.symlink_to("test_b.md")
        git(self.root, "add", "crate/test_laziness/input_link")
        commit(self.root, "change tracked symlink target")
        self.assertGreater(watched.stat().st_mtime_ns, OLD)

        restore_mtimes.main(self.manifest)

        self.assertEqual(os.readlink(link), "test_b.md")
        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertEqual((watched / "test_a.md").stat().st_mtime_ns, OLD)
        self.assertEqual((watched / "test_b.md").stat().st_mtime_ns, OLD)

    def test_dirty_tracked_checkout_does_not_rewrite_times_or_manifest(self):
        self.first_build()
        self.fresh_clone()
        manifest_before = Path(self.manifest).read_bytes()
        tracked = self.root / "crate/test_laziness/test_a.md"
        tracked.write_text("unstaged worktree content")
        file_mtime = tracked.stat().st_mtime_ns
        directory_mtime = (self.root / "crate/test_laziness").stat().st_mtime_ns

        restore_mtimes.main(self.manifest)

        self.assertEqual(tracked.stat().st_mtime_ns, file_mtime)
        self.assertEqual(
            (self.root / "crate/test_laziness").stat().st_mtime_ns,
            directory_mtime,
        )
        self.assertEqual(Path(self.manifest).read_bytes(), manifest_before)

    def test_unstaged_tracked_symlink_change_is_not_certified(self):
        watched = self.root / "crate/test_laziness"
        link = watched / "input_link"
        link.symlink_to("test_a.md")
        git(self.root, "add", "crate/test_laziness/input_link")
        commit(self.root, "add tracked input symlink")
        self.first_build()
        self.fresh_clone()
        restore_mtimes.main(self.manifest)
        manifest_before = Path(self.manifest).read_bytes()

        link.unlink()
        link.symlink_to("test_b.md")
        directory_mtime = watched.stat().st_mtime_ns
        self.assertGreater(directory_mtime, OLD)

        restore_mtimes.main(self.manifest)

        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertEqual(os.readlink(link), "test_b.md")
        self.assertEqual(Path(self.manifest).read_bytes(), manifest_before)

    def test_gitlink_directory_is_opaque_without_following_children(self):
        self.first_build()
        self.fresh_clone()
        watched = self.root / "crate/test_laziness"
        gitlink = watched / "linked_checkout"
        gitlink.mkdir()
        target = self.root.parent / f"{self.root.name}-gitlink-target"
        target.write_text("outside submodule input")
        self.external_targets.append(target)
        target_mtime = target.stat().st_mtime_ns
        (gitlink / "outside_link").symlink_to(target)

        source_sha = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=self.root, text=True
        ).strip()
        git(
            self.root,
            "update-index",
            "--add",
            "--cacheinfo",
            f"160000,{source_sha},crate/test_laziness/linked_checkout",
        )
        git(self.root, "config", "submodule.crate/test_laziness/linked_checkout.ignore", "all")
        commit(self.root, "add opaque gitlink fixture")
        self.assertGreater(watched.stat().st_mtime_ns, OLD)

        original_scandir = os.scandir

        def no_submodule_walk(path):
            self.assertNotEqual(Path(path).absolute(), gitlink.absolute())
            return original_scandir(path)

        with patch.object(restore_mtimes.os, "scandir", side_effect=no_submodule_walk):
            restore_mtimes.main(self.manifest)

        signatures = json.loads(Path(self.manifest).read_text())[restore_mtimes.DIRECTORIES]
        self.assertNotIn("crate/test_laziness", signatures)
        self.assertNotIn("crate", signatures)
        self.assertGreater(watched.stat().st_mtime_ns, OLD)
        self.assertEqual(target.stat().st_mtime_ns, target_mtime)
        self.assertEqual((self.root / "crate/lib.rs").stat().st_mtime_ns, OLD)

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
