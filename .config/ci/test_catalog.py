"""Check exact committed catalog bytes, source boundaries and command safety."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from catalog import read_catalog


class CatalogTests(unittest.TestCase):
    def fixture(self, root, cases=None, tracked=True):
        subprocess.run(['git', 'init', '-q', str(root)], check=True)
        path = root / 'tests/surface/cases.json'
        path.parent.mkdir(parents=True)
        path.write_text(json.dumps({'cases': cases if cases is not None else [{'name': 'example'}]}) + '\n')
        (root / 'tracked').write_text('unchanged\n')
        subprocess.run(['git', '-C', str(root), 'add', '.' if tracked else 'tracked'], check=True)
        subprocess.run(['git', '-C', str(root), '-c', 'user.name=Catalog Test', '-c',
                        'user.email=catalog@example.invalid', '-c', 'commit.gpgsign=false',
                        'commit', '-qm', 'fixture'], check=True)
        sha = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
        return sha, path

    def test_bound_catalog_names_exact_blob_and_legacy_shape_stays_available(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sha, path = self.fixture(root)
            bound = read_catalog(root, sha)
            self.assertEqual(bound['source_sha'], sha)
            self.assertEqual(bound['schema'], 1)
            self.assertEqual(bound['catalog_sha256'], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(bound['tiers'], read_catalog(root))
            self.assertEqual(bound['tiers']['integration'], ['example'])

    def test_wrong_head_dirty_checkout_and_untracked_catalog_cannot_attest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sha, path = self.fixture(root)
            with self.assertRaisesRegex(ValueError, 'differs'):
                read_catalog(root, 'b' * 40)
            path.write_text('{"cases":[{"name":"changed"}]}')
            with self.assertRaisesRegex(ValueError, 'clean'):
                read_catalog(root, sha)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sha, _ = self.fixture(root, tracked=False)
            with self.assertRaises(subprocess.CalledProcessError):
                read_catalog(root, sha)

    def test_malformed_duplicate_and_command_like_names_are_refused(self):
        for cases in ([], [{'name': 'same'}, {'name': 'same'}], [{'name': '../escape'}],
                      [{'name': 'case;run-command'}], [None], [{'name': 'x'}] * 201):
            with self.subTest(cases=str(cases)[:80]), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.fixture(root, cases)
                with self.assertRaises(ValueError):
                    read_catalog(root)

    def test_public_command_reads_only_git_without_build_tools(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            sha, _ = self.fixture(root)
            ci = root / '.config/ci'
            ci.mkdir(parents=True)
            for name in ('test.py', 'catalog.py', 'source_identity.py'):
                shutil.copyfile(Path(__file__).with_name(name), ci / name)
            subprocess.run(['git', '-C', str(root), 'add', '.config'], check=True)
            subprocess.run(['git', '-C', str(root), '-c', 'user.name=Catalog Test', '-c',
                            'user.email=catalog@example.invalid', '-c', 'commit.gpgsign=false',
                            'commit', '-qm', 'runner'], check=True)
            sha = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'], text=True).strip()
            tools = root / 'tools'
            tools.mkdir()
            (tools / 'git').symlink_to(shutil.which('git'))
            result = subprocess.run([sys.executable, str(ci / 'test.py'), 'list', '--source-sha', sha],
                                    env=dict(os.environ, PATH=str(tools)), capture_output=True, text=True, timeout=30)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['source_sha'], sha)


if __name__ == '__main__':
    unittest.main()
