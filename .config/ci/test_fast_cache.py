#!/usr/bin/env python3
"""Filesystem safety controls for the CI cache's mount-preserving eviction."""
from pathlib import Path
from contextlib import redirect_stdout
from io import StringIO
import os
import tempfile
import unittest

from fast_cache import clear_cache, ensure_cargo_home, observe_cargo_home_parent


class FastCacheTest(unittest.TestCase):
    def test_missing_cargo_home_parent_is_recreated_as_the_owned_direct_child(self):
        with tempfile.TemporaryDirectory() as parent:
            root = Path(parent) / 'fast-cache'
            root.mkdir()
            sibling = root / 'targets'
            sibling.mkdir()
            sentinel = sibling / 'keep'
            sentinel.write_bytes(b'preserve')
            cargo_home = root / 'cargo-home'
            lock = cargo_home / '.package-cache'
            with self.assertRaises(FileNotFoundError):
                os.open(lock, os.O_CREAT | os.O_RDWR, 0o600)

            ensure_cargo_home(root, cargo_home)
            home_identity = cargo_home.stat().st_ino
            fd = os.open(lock, os.O_CREAT | os.O_RDWR, 0o600)
            os.close(fd)
            ensure_cargo_home(root, cargo_home)

            self.assertEqual(cargo_home.stat().st_ino, home_identity)
            self.assertEqual(sentinel.read_bytes(), b'preserve')

    def test_cargo_home_admission_rejects_symlink_and_wrong_child_without_touching_target(
        self,
    ):
        with tempfile.TemporaryDirectory() as parent:
            root = Path(parent) / 'fast-cache'
            root.mkdir()
            foreign = Path(parent) / 'foreign'
            foreign.mkdir()
            sentinel = foreign / 'keep'
            sentinel.write_bytes(b'foreign')
            (root / 'cargo-home').symlink_to(foreign, target_is_directory=True)
            with self.assertRaises(ValueError):
                ensure_cargo_home(root, root / 'cargo-home')
            with self.assertRaises(ValueError):
                ensure_cargo_home(root, foreign)
            self.assertEqual(sentinel.read_bytes(), b'foreign')

    def test_cargo_home_admission_refuses_filesystem_root(self):
        with self.assertRaises(ValueError):
            ensure_cargo_home('/', '/cargo-home')

    def test_unconfigured_fast_cache_reports_missing_parent_without_creating_it(self):
        with tempfile.TemporaryDirectory() as parent:
            home = Path(parent) / 'absent' / 'cargo-home'
            output = StringIO()
            with redirect_stdout(output):
                observe_cargo_home_parent(home)
            self.assertIn('state=skipped reason=cache-root-unset parent=absent', output.getvalue())
            self.assertFalse(home.parent.exists())

    def test_eviction_preserves_root_inode_and_removes_hidden_nested_outputs(self):
        with tempfile.TemporaryDirectory() as parent:
            root = Path(parent) / 'cache'
            (root / 'targets' / 'debug').mkdir(parents=True)
            (root / 'targets' / 'debug' / 'output').write_bytes(b'regenerable')
            (root / '.source-mtimes.json').write_text('{}')
            before = root.stat()
            clear_cache(root)
            self.assertEqual(list(root.iterdir()), [])
            self.assertEqual((root.stat().st_dev, root.stat().st_ino),
                             (before.st_dev, before.st_ino))
            clear_cache(root)
            self.assertEqual(root.stat().st_ino, before.st_ino)

    def test_linked_external_tree_and_neighbor_are_untouched(self):
        with tempfile.TemporaryDirectory() as parent:
            root = Path(parent) / 'cache'
            root.mkdir()
            external = Path(parent) / 'neighbor'
            external.mkdir()
            sentinel = external / 'keep'
            sentinel.write_text('not a cache output')
            (root / 'external-directory').symlink_to(external, target_is_directory=True)
            (root / 'external-file').symlink_to(sentinel)
            clear_cache(root)
            self.assertEqual(sentinel.read_text(), 'not a cache output')
            self.assertEqual(list(root.iterdir()), [])

    def test_symlinked_root_and_filesystem_root_are_refused(self):
        with tempfile.TemporaryDirectory() as parent:
            target = Path(parent) / 'target'
            target.mkdir()
            sentinel = target / 'keep'
            sentinel.write_text('keep')
            link = Path(parent) / 'cache-link'
            link.symlink_to(target, target_is_directory=True)
            with self.assertRaises(OSError):
                clear_cache(link)
            self.assertEqual(sentinel.read_text(), 'keep')
        for path in ('/', '/.', '/../'):
            with self.subTest(path=path), self.assertRaises(ValueError):
                clear_cache(path)


if __name__ == '__main__':
    unittest.main()
