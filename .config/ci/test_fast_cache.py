#!/usr/bin/env python3
"""Filesystem safety controls for the CI cache's mount-preserving eviction."""
from pathlib import Path
import tempfile
import unittest

from fast_cache import clear_cache


class FastCacheTest(unittest.TestCase):
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
