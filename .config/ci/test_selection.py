#!/usr/bin/env python3
"""Selection must fail closed for renames, incomplete history and local changes."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from selection import preview, select


class SelectionTests(unittest.TestCase):
    def test_explicit_docs_only_and_conservative_fallback(self):
        self.assertEqual(select(['docs/guide.md', 'README.md', 'LICENSE'], ['one'])['tier'], 'T0')
        for paths in (None, [], ['notes.md'], ['docs.md'], ['docs/guide.md', 'crates/lib.rs'],
                      ['../docs/guide.md'], ['/docs/guide.md']):
            with self.subTest(paths=paths):
                result = select(paths, ['one'])
                self.assertEqual(result['integration_cases'], ['one'])
                self.assertEqual(result['tier'], 'T1+T2')

    def git(self, *args):
        return subprocess.check_output(['git', '-C', str(self.root), *args], stderr=subprocess.PIPE).decode().strip()

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'Selection control')
        self.git('config', 'user.email', 'selection@example.invalid')
        (self.root / 'tests/surface').mkdir(parents=True)
        (self.root / 'tests/surface/cases.json').write_text(json.dumps({'cases': [{'name': 'one'}]}))
        (self.root / 'source.rs').write_text('fn source() {}\n')
        self.git('add', '.')
        self.git('commit', '-qm', 'initial')
        self.base = self.git('rev-parse', 'HEAD')

    def test_source_to_docs_rename_keeps_deleted_source(self):
        (self.root / 'docs').mkdir()
        self.git('mv', 'source.rs', 'docs/example.md')
        self.git('commit', '-qm', 'rename source into docs')
        result = preview(self.root, self.base)
        self.assertEqual(result['tier'], 'T1+T2')
        self.assertEqual(result['paths'], ['docs/example.md', 'source.rs'])
        self.assertEqual(result['head_sha'], self.git('rev-parse', 'HEAD'))

    def test_nul_delimited_names_and_worktree_changes(self):
        (self.root / 'docs').mkdir()
        (self.root / 'docs/with\nnewline.md').write_text('docs')
        self.git('add', '.')
        self.git('commit', '-qm', 'docs')
        self.assertEqual(preview(self.root, self.base)['tier'], 'T0')
        (self.root / 'new.rs').write_text('untracked product source')
        self.assertEqual(preview(self.root, self.base, worktree=True)['tier'], 'T1+T2')
        (self.root / 'new.rs').unlink()
        (self.root / 'source.rs').write_text('unstaged product change')
        self.assertEqual(preview(self.root, self.base, worktree=True)['tier'], 'T1+T2')
        self.git('add', 'source.rs')
        self.assertEqual(preview(self.root, self.base, worktree=True)['tier'], 'T1+T2')

    def test_missing_history_and_empty_diff_never_skip(self):
        for base in (self.base, 'missing-ref', '--help'):
            self.assertEqual(preview(self.root, base)['integration_cases'], ['one'])

    def test_merge_base_ignores_unrelated_new_base_commits(self):
        self.git('checkout', '-qb', 'base-advanced')
        (self.root / 'unrelated.rs').write_text('new base change')
        self.git('add', '.')
        self.git('commit', '-qm', 'advance base')
        advanced = self.git('rev-parse', 'HEAD')
        self.git('checkout', '-qb', 'feature', self.base)
        (self.root / 'README.md').write_text('feature docs')
        self.git('add', '.')
        self.git('commit', '-qm', 'feature docs')
        result = preview(self.root, advanced)
        self.assertEqual(result['paths'], ['README.md'])
        self.assertEqual(result['merge_base'], self.base)
        self.assertEqual(result['tier'], 'T0')


if __name__ == '__main__':
    unittest.main()
