"""Finite local-development custody checks; no signing or product build."""
import argparse
import hashlib
import json
import plistlib
import tempfile
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import record_macos_stylo_development_bundle as actor
import verify_macos_stylo_development_bundle as verifier


class DevelopmentBundleTests(unittest.TestCase):
    def exercise_actor(self, tamper_worker=False, tamper_sqlite=False, wrong_cli_version=False, workspace_tag='0.2.0'):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source, cargo_home, target, build, output = [root / n for n in ('source', 'cargo', 'target', 'build', 'output')]
            for path in (source, cargo_home, target, build):
                path.mkdir()
            for rel, data in [('apps/desktop/macos/Info.plist', plistlib.dumps({'CFBundleExecutable':'Nudox'})), ('apps/desktop/macos/Nudox.icns', b'icon'),
                              ('vendor/stylo-native/LICENSE', b'license'), ('vendor/selectors-native/LICENSE', b'license'),
                              ('vendor/syntaqlite-native/LICENSE', b'license'), ('vendor/syntaqlite-native/syntaqlite-common/LICENSE', b'license'),
                              ('vendor/syntaqlite-native/syntaqlite-syntax/LICENSE', b'license'), ('vendor/syntaqlite-native/syntaqlite/LICENSE', b'license'),
                              ('Cargo.toml', f'[workspace.package]\nversion = "{workspace_tag}"\n'.encode()),
                              ('tools/package/record_macos_stylo_development_bundle.py', b'actor'),
                              ('tools/package/macos_stylo_companion.py', b'checker'),
                              ('tools/package/sqlite_macos_development.py', b'sqlite checker')]:
                path = source / rel
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(data)
            cargo = root / 'bin/cargo'
            cargo.parent.mkdir()
            cargo.write_bytes(b'cargo')
            args = argparse.Namespace(commit='a' * 40, jobs=2, timeout_seconds=60,
                source_checkout=source, target='aarch64-apple-darwin', cargo_home=cargo_home,
                target_dir=target, build_dir=build, output_dir=output, cargo=cargo, rustc=root/'rustc',
                cargo_sha256='0'*64, rustc_sha256='0'*64)
            snapshot = {'path': source, 'clean': True, 'commit': 'a'*40, 'tree':'b'*40, 'cargo_lock_sha256':'c'*64}
            release = target / args.target / 'release'
            release.mkdir(parents=True)
            anchor = '1' * 64
            calls = []
            def execute(command, cwd, environment, logs, deadline, maximum):
                calls.append((command, dict(environment)))
                if command == actor.WORKER:
                    (release/'backend-stylo-worker').write_bytes(b'worker')
                elif command == actor.OBSERVER:
                    (release/'examples').mkdir()
                    (release/'examples/macos_stylo_identity_observer').write_bytes(b'observer')
                else:
                    self.assertEqual(environment['NUDOX_BUILD_STYLO_WORKER_ID'], anchor)
                    self.assertEqual(environment['NUDOX_BUILD_SQLITE_WORKER_ID'], '2'*64)
                    self.assertEqual(environment['NUDOX_BUILD_SQLITE_BASIS_JSON'], '{"basis":true}')
                    for name in ('backend-desktop', 'backend-locald', 'backend-mcp', 'backend-cli'):
                        (release/name).write_bytes(name.encode())
                return {'exit':0, 'process_group_retired':True}
            def run_tool(command, **kwargs):
                if command[0].endswith('macos_stylo_identity_observer'):
                    return json.dumps({'state':'observed-helper','identity':anchor}).encode()
                if command[0].endswith('/backend-cli') and command[1:] == ['--version']:
                    return b'backend 9.9.9\n' if wrong_cli_version else f'backend {workspace_tag}\n'.encode()
                if command[0].endswith('/backend-cli') and command[1:] == ['--help']:
                    return b'backend help\n'
                if command[0] == '/usr/bin/ditto':
                    Path(command[-1]).write_bytes(b'archive')
                if tamper_worker and command[:4] == ['/usr/bin/codesign','--force','--sign','-'] and command[-1].endswith('.app'):
                    (output/'Nudox Development.app/Contents/MacOS/backend-stylo-worker').write_bytes(b'tampered')
                if tamper_sqlite and command[:4] == ['/usr/bin/codesign','--force','--sign','-'] and command[-1].endswith('.app'):
                    (output/'Nudox Development.app/Contents/MacOS/backend-sqlite-worker').write_bytes(b'tampered')
                return b''
            def inspect(path, target, **kwargs):
                size, checksum = actor.companion.digest_regular(path)
                return {'bytes':size, 'sha256':checksum, 'architecture':'arm64', 'system_loads':['/usr/lib/libSystem.B.dylib']}
            def sqlite_prepare(**kwargs):
                (kwargs['macos']/'backend-sqlite-worker').write_bytes(b'sqlite')
                return {'artifact_identity':'2'*64, 'owner_environment':{'NUDOX_BUILD_SQLITE_WORKER_ID':'2'*64,
                                                                          'NUDOX_BUILD_SQLITE_BASIS_JSON':'{"basis":true}'}}
            def sqlite_verify(record, **kwargs):
                if (output/'Nudox Development.app/Contents/MacOS/backend-sqlite-worker').read_bytes() != b'sqlite':
                    raise ValueError('outer signing changed SQLite worker')
            with patch.object(actor.sys, 'platform', 'darwin'), patch.object(actor.platform, 'machine', return_value='arm64'), \
                 patch.object(actor, 'source_snapshot', return_value=snapshot), patch.object(actor.common, 'ROOT', source), \
                 patch.object(actor.common, 'directory', side_effect=lambda p: p), \
                 patch.object(actor.common.runtime, 'output_path', return_value=output), \
                 patch.object(actor.common, 'tool', side_effect=lambda args, name: {'name':name}), \
                 patch.object(actor, 'apple_provenance', return_value={'sdk':'test'}), \
                 patch.object(actor.shutil, 'which', return_value=str(cargo)), \
                 patch.object(actor.common, 'observe_build_tool', return_value={'sha256':'test'}), \
                 patch.object(actor.common, 'execute', side_effect=execute), \
                 patch.object(actor.sqlite, 'prepare', side_effect=sqlite_prepare), \
                 patch.object(actor.sqlite, 'verify_after_signing', side_effect=sqlite_verify), \
                 patch.object(actor.companion, 'run_tool', side_effect=run_tool), \
                 patch.object(actor.companion, 'inspect_system_executable', side_effect=inspect):
                if tamper_worker:
                    with self.assertRaisesRegex(ValueError, 'changed the admitted worker'):
                        actor.capture(args)
                    self.assertFalse((output/'development-bundle-receipt.json').exists())
                elif tamper_sqlite:
                    with self.assertRaisesRegex(ValueError, 'changed SQLite worker'):
                        actor.capture(args)
                    self.assertFalse((output/'development-bundle-receipt.json').exists())
                elif wrong_cli_version:
                    with self.assertRaisesRegex(ValueError, 'CLI version or help mismatch'):
                        actor.capture(args)
                    self.assertFalse((output/'development-bundle-receipt.json').exists())
                else:
                    receipt_path = actor.capture(args)
                    receipt = json.loads(receipt_path.read_bytes())
                    self.assertEqual(receipt['worker_anchor'], anchor)
                    self.assertEqual(calls[-1][0], actor.PRODUCT)
                    self.assertIn('backend-desktop', actor.PRODUCT)
                    self.assertIn('backend-locald', actor.PRODUCT)
                    self.assertEqual(receipt['outer_signing'][1:4], ['--force','--sign','-'])
                    self.assertEqual(len(receipt['bundle_digest']['files']), 14)
                    self.assertEqual(receipt['archive']['bytes'], len(b'archive'))
                    self.assertEqual(receipt['cli_checks']['workspace_version'], workspace_tag)
                    self.assertEqual(set(receipt['native_images']), {'Nudox','backend-locald','backend-mcp','backend-cli','backend-stylo-worker','backend-sqlite-worker'})
                    plist = plistlib.loads((output/'Nudox Development.app/Contents/Info.plist').read_bytes())
                    self.assertEqual(plist['CFBundleIdentifier'], 'dev.nudox.support.development')
                    self.assertEqual(plist['CFBundleShortVersionString'], '0.2.0')
                    self.assertEqual(plist['CFBundleVersion'], '0.2.0')
                    receipt_hash = hashlib.sha256(receipt_path.read_bytes()).hexdigest()
                    app = output/'Nudox Development.app'
                    self.assertEqual(verifier.verify(app, receipt_path, receipt_hash)['worker_anchor'], anchor)
                    with self.assertRaisesRegex(ValueError, 'expected hash'):
                        verifier.verify(app, receipt_path, '0'*64)
                    (app/'Contents/Resources/Nudox.icns').write_bytes(b'tampered icon')
                    with self.assertRaisesRegex(ValueError, 'bundle bytes'):
                        verifier.verify(app, receipt_path, receipt_hash)

    def test_anchored_desktop_and_owner_and_preserved_worker(self):
        self.exercise_actor()

    def test_outer_signing_worker_mutation_refuses_receipt(self):
        self.exercise_actor(tamper_worker=True)

    def test_outer_signing_sqlite_mutation_refuses_receipt(self):
        self.exercise_actor(tamper_sqlite=True)

    def test_cli_version_mismatch_refuses_receipt(self):
        self.exercise_actor(wrong_cli_version=True)

    def test_prerelease_retains_full_cli_version_and_numeric_bundle_version(self):
        self.exercise_actor(workspace_tag='0.2.0-rc.1')

    def test_bundle_digest_binds_content_modes_and_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as temporary:
            app = Path(temporary) / 'App.app'
            app.mkdir()
            f = app / 'worker'
            f.write_bytes(b'worker')
            f.chmod(0o700)
            first = actor.bundle_digest(app, time.monotonic() + 10)
            f.write_bytes(b'changed')
            second = actor.bundle_digest(app, time.monotonic() + 10)
            self.assertNotEqual(first['sha256'], second['sha256'])
            f.chmod(0o600)
            third = actor.bundle_digest(app, time.monotonic() + 10)
            self.assertNotEqual(second['sha256'], third['sha256'])
            (app / 'symlink').symlink_to(f)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                actor.bundle_digest(app, time.monotonic() + 10)

    def test_wrong_source_refuses_before_build_or_signing(self):
        args = argparse.Namespace(commit='a' * 40, jobs=2, timeout_seconds=60,
                                  source_checkout=Path('/tmp/source'))
        with patch.object(actor.sys, 'platform', 'darwin'), \
             patch.object(actor, 'source_snapshot', return_value={'path': Path('/tmp/source'), 'clean': True, 'commit': 'b' * 40}), \
             patch.object(actor.common, 'ROOT', Path('/tmp/source')), \
             patch.object(actor.companion, 'run_tool') as run_tool:
            with self.assertRaisesRegex(ValueError, 'exact clean checkout'):
                actor.capture(args)
            run_tool.assert_not_called()

    def test_wrong_host_refuses_before_build_or_signing(self):
        args = argparse.Namespace(commit='a' * 40, jobs=2, timeout_seconds=60,
                                  source_checkout=Path('/tmp/source'), target='x86_64-apple-darwin')
        with patch.object(actor.sys, 'platform', 'darwin'), \
             patch.object(actor.platform, 'machine', return_value='arm64'), \
             patch.object(actor, 'source_snapshot', return_value={'path': Path('/tmp/source'), 'clean': True, 'commit': 'a' * 40}), \
             patch.object(actor.common, 'ROOT', Path('/tmp/source')), \
             patch.object(actor.companion, 'run_tool') as run_tool:
            with self.assertRaisesRegex(ValueError, 'native host'):
                actor.capture(args)
            run_tool.assert_not_called()


if __name__ == '__main__':
    unittest.main()
