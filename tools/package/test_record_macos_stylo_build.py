"""Finite refusal and custody checks for macOS Stylo build evidence."""
import argparse
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import record_macos_stylo_build as actor

class ActorTests(unittest.TestCase):
    def test_refuses_adhoc_or_unmatched_target_before_build(self):
        args = argparse.Namespace(target='aarch64-apple-darwin', team_id='A1B2C3D4E5', signing_identity='-', jobs=2, timeout_seconds=5, max_log_bytes=1024)
        with patch.object(actor.platform, 'system', return_value='Darwin'), patch.object(actor.platform, 'machine', return_value='arm64'):
            with self.assertRaisesRegex(ValueError, 'non-ad-hoc'):
                actor.capture(args)
        args.signing_identity = 'Developer ID Application: Example'
        with patch.object(actor.platform, 'system', return_value='Darwin'), patch.object(actor.platform, 'machine', return_value='x86_64'):
            with self.assertRaisesRegex(ValueError, 'target'):
                actor.capture(args)

    def test_source_snapshot_ignores_git_and_toolchain_redirection(self):
        import os
        import subprocess
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source, foreign = root/'source', root/'foreign'
            for path in (source, foreign):
                path.mkdir()
                env = {**os.environ, 'GIT_CONFIG_GLOBAL':'/dev/null', 'GIT_CONFIG_SYSTEM':'/dev/null', 'GIT_CONFIG_NOSYSTEM':'1'}
                for args in (('init',), ('config','user.name','Fixture'), ('config','user.email','fixture@example.invalid')):
                    subprocess.run(['/usr/bin/git','-C',str(path),*args], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
                (path/'Cargo.lock').write_text(path.name)
                subprocess.run(['/usr/bin/git','-C',str(path),'add','Cargo.lock'], env=env, check=True)
                subprocess.run(['/usr/bin/git','-C',str(path),'commit','-m','fixture'], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True)
            expected = subprocess.check_output(['/usr/bin/git','-C',str(source),'rev-parse','HEAD'], text=True).strip()
            redirect = {'GIT_DIR':str(foreign/'.git'), 'GIT_WORK_TREE':str(foreign), 'DEVELOPER_DIR':'/missing', 'PATH':'/missing'}
            with patch.dict(os.environ, redirect):
                captured = actor.source_snapshot(source)
                self.assertEqual(captured['commit'], expected)
                self.assertTrue(captured['clean'])
                self.assertEqual(os.environ['DEVELOPER_DIR'], '/missing')
                self.assertEqual(os.environ['PATH'], '/missing')

    def test_copy_rejects_symlink_and_scans_chunk_boundary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            src = root/'worker'
            src.write_bytes(b'worker')
            link = root/'link'
            link.symlink_to(src)
            with self.assertRaises(ValueError):
                actor.copy_exact(link, root/'out')
            private = root/'private'
            marker = str(private).encode()
            src.write_bytes(b'x' * (1024*1024-len(marker)//2) + marker + b'z')
            with self.assertRaisesRegex(ValueError, 'private build path'):
                actor.reject_private_paths(src, [private])

if __name__ == '__main__':
    unittest.main()
