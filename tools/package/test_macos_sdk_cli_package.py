"""Ordinary byte/installer controls, explicitly not native/signing/SDK proof."""
import copy
from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import install_macos_preview as installer
import macos_sdk_cli_package as package
import macos_release
import compose_forgejo_checkpoint as compose
from test_compose_forgejo_checkpoint import synthetic_inputs
import test_install_preview as legacy_fixtures


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, sort_keys=True)+'\n')
    path.chmod(0o644)


def fixture(parent, revision='a'*40):
    """Deliberately non-executable bytes; controls never run/sign these fixtures."""
    app = parent/'Nudox.app'
    source = {'git_revision': revision, 'git_tree': 'b'*40, 'cargo_lock_sha256': hashlib.sha256(b'unit Cargo.lock fixture').hexdigest(), 'working_tree': 'clean'}
    notices = {'node': 'typescript/node/LICENSE', 'typescript': 'typescript/node_modules/typescript/LICENSE.txt',
               'typescript_third_party': 'typescript/node_modules/typescript/ThirdPartyNoticeText.txt'}
    values = {'typescript/node/bin/node': b'ordinary fixture, not Node',
              'typescript/node_modules/typescript/package.json': b'{"name":"typescript","version":"5.9.3"}',
              'typescript/node_modules/typescript/lib/typescript.js': b'ordinary fixture, not compiler API',
              'typescript/node_modules/typescript/bin/tsc': b'ordinary fixture, not tsc',
              **{name: b'ordinary fixture notice' for name in notices.values()}}
    helper = app/'Contents/Resources/Helpers'
    for name, raw in values.items():
        path = helper/name;path.parent.mkdir(parents=True, exist_ok=True);path.write_bytes(raw)
        path.chmod(0o755 if name == 'typescript/node/bin/node' else 0o644)
    files = {name: hashlib.sha256(raw).hexdigest() for name, raw in values.items()}
    receipt = {'schema': 1, 'source': source, 'target': 'aarch64-apple-darwin', 'files': files,
               'file_modes': {name: 0o755 if name == 'typescript/node/bin/node' else 0o644 for name in files},
               'tools': {'node': {'version': 'v24.18.0', 'sha256': files['typescript/node/bin/node']}, 'typescript': {'version': '5.9.3'}},
               'notices': notices}
    receipt_path = app/'Contents/Resources/Build Evidence/compiler-helpers-receipt.json'
    write_json(receipt_path, receipt)
    for name in (*installer.REQUIRED_BINARIES, 'backend-desktop', 'Nudox'):
        path = app/'Contents/MacOS'/name;path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b'ordinary fixture, not Mach-O '+name.encode());path.chmod(0o755)
    build = {'schema': 1, 'source': source, 'target': 'aarch64-apple-darwin',
             'executables': {name: installer._file_sha256(app/'Contents/MacOS'/name) for name in (*installer.REQUIRED_BINARIES, 'backend-desktop')}}
    build_path = app/'Contents/Resources/Build Evidence/application-build-receipt.json'
    write_json(build_path, build)
    inventory = {}
    for path in app.rglob('*'):
        if path.is_file():
            inventory[path.relative_to(app).as_posix()] = {'kind': 'file', 'sha256': installer._file_sha256(path),
                                                         'size_bytes': path.stat().st_size, 'mode': path.stat().st_mode & 0o777}
    evidence = {'schema': 1, 'product': 'Nudox', 'source': source,
                'target': {'triple': 'aarch64-apple-darwin', 'architecture': 'arm64'}, 'files': inventory,
                'bundle': {'identifier': 'dev.nudox.desktop'},
                'application_build': {'receipt_sha256': installer._file_sha256(build_path), 'executables': build['executables']},
                'compiler_helpers': {'assembly_mode': 'typescript-sdk-only', 'files': files,
                                     'tools': receipt['tools'], 'notices': notices,
                                     'receipt_sha256': installer._file_sha256(receipt_path),
                                     'runtime_probe_status': 'passed post-sign Node and Compiler API probes'},
                'sdk_finalization': {'native_probes_stage': 'after inner signing, before outer app seal'}}
    write_json(app/'Contents/Resources/build-manifest.json', evidence)
    # Existing SDK finalization explicitly excludes the subsequent outer seal.
    (app/'Contents/_CodeSignature').mkdir();(app/'Contents/_CodeSignature/CodeResources').write_bytes(b'ordinary signature fixture')
    return app


class SdkCliControls(unittest.TestCase):
    def setUp(self):
        saved_umask = os.umask(0o022)
        self.addCleanup(os.umask, saved_umask)
        self.temp = tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name);self.app = fixture(self.root)
        self.output = self.root/'candidate'

    def validate_mac(self, source, date):
        with patch.object(compose, 'source_file', return_value=b'unit Cargo.lock fixture'):
            return compose.validate_mac_sdk_package(self.output, source, date)

    def derive(self):
        return package.derive(self.app, self.output, 'a'*40)

    def test_complete_app_modes_aliases_repeat_and_sibling_daemon(self):
        release = self.derive()
        entry = {'version': '0.0.0', 'source_sha': 'a'*40, 'tag': 'checkpoint-20261011-aaaaaaaaaa-macos-arm64', 'asset': release['asset']}
        identity = {'sha256': release['sha256']};prefix = self.root/'home/.local'
        with patch.object(installer, 'check_startup') as startup:
            installer.install(prefix, entry, identity, self.output/release['asset'])
            self.assertTrue(installer.reuse_existing_install(prefix, entry, identity))
        self.assertEqual([call.args[1] for call in startup.call_args_list], [installer.SDK_BIN]*2)
        active = prefix/'lib/nudox/current'
        self.assertEqual((prefix/'bin/nudox').resolve().parent, (active/installer.SDK_BIN).resolve())
        self.assertEqual((prefix/'bin/nudox-locald').resolve().parent, (prefix/'bin/nudox').resolve().parent)
        for alias, target in installer.command_links(prefix/'lib/nudox').items():
            self.assertEqual(os.readlink(prefix/'bin'/alias), target)
        self.assertEqual((active/'Nudox.app/Contents/Resources/Helpers/typescript/node/bin/node').stat().st_mode & 0o777, 0o755)
        self.assertEqual(installer._file_sha256(active/'Nudox.app/Contents/Resources/build-manifest.json'), release['build_manifest_sha256'])
        (active/'Nudox.app/Contents/Resources/Helpers/typescript/node/LICENSE').write_bytes(b'changed')
        with patch.object(installer, 'check_startup'):
            with self.assertRaisesRegex(installer.InstallError, 'integrity/startup'):
                installer.reuse_existing_install(prefix, entry, identity)

    def test_verified_updates_switch_flat_to_app_and_back(self):
        release = self.derive()
        legacy_archive = legacy_fixtures.Tests().archive(self.root)
        legacy_digest = installer._file_sha256(legacy_archive)
        prefix = self.root/'transition-home/.local'
        entry = {'version': '0.0.0', 'source_sha': 'a'*40, 'tag': 'checkpoint-20261011-aaaaaaaaaa-macos-arm64', 'asset': release['asset']}
        with patch.object(installer, 'check_startup'):
            installer.install(prefix, entry, {'sha256': legacy_digest}, legacy_archive)
            self.assertIn('/current/bin/', os.readlink(prefix/'bin/nudox'))
            entry = dict(entry, tag='checkpoint-20261012-aaaaaaaaaa-macos-arm64')
            installer.install(prefix, entry, {'sha256': release['sha256']}, self.output/release['asset'])
            self.assertEqual(os.readlink(prefix/'lib/nudox/current/bin'), installer.SDK_BIN)
            self.assertEqual((prefix/'bin/nudox').resolve().parent.name, 'MacOS')
            self.assertTrue(installer.reuse_existing_install(prefix, entry, {'sha256': release['sha256']}))
            entry = dict(entry, tag='checkpoint-20261013-aaaaaaaaaa-macos-arm64')
            installer.install(prefix, entry, {'sha256': legacy_digest}, legacy_archive)
            self.assertIn('/current/bin/', os.readlink(prefix/'bin/nudox'))
            self.assertTrue(installer.reuse_existing_install(prefix, entry, {'sha256': legacy_digest}))

    def test_source_pending_or_wrong_target_refuses_before_archive(self):
        path = self.app/'Contents/Resources/build-manifest.json'
        original = json.loads(path.read_text())
        for change in ('source', 'target', 'probe'):
            bad = copy.deepcopy(original)
            if change == 'source': bad['source']['git_revision'] = 'd'*40
            elif change == 'target': bad['target']['triple'] = 'x86_64-apple-darwin'
            else: bad['compiler_helpers']['runtime_probe_status'] = 'pending post-sign native probes'
            write_json(path, bad)
            with self.assertRaises(installer.InstallError): self.derive()
            self.assertFalse(self.output.exists())

    def test_extra_missing_symlink_and_mode_refuse(self):
        self.derive()
        archive = self.output/next(p.name for p in self.output.glob('*.tar.gz'))
        stage = self.root/'stage';stage.mkdir()
        installer.safe_extract(archive, stage)
        manifest = json.loads((stage/'manifest.json').read_text())
        installer.verify_sdk_package(stage, manifest, 'a'*40)
        node = stage/'Nudox.app/Contents/Resources/Helpers/typescript/node/bin/node'
        node.chmod(0o644)
        with self.assertRaisesRegex(installer.InstallError, 'mode mismatch'): installer.verify_sdk_package(stage, manifest, 'a'*40)
        node.chmod(0o755)
        extra = stage/'Nudox.app/extra';extra.write_text('unreceipted')
        with self.assertRaisesRegex(installer.InstallError, 'file set'): installer.verify_sdk_package(stage, manifest, 'a'*40)
        extra.unlink();extra.mkdir()
        with self.assertRaisesRegex(installer.InstallError, 'directory'): installer.verify_sdk_package(stage, manifest, 'a'*40)
        extra.rmdir();node.unlink();node.symlink_to(self.app/'Contents/Resources/Helpers/typescript/node/bin/node')
        with self.assertRaises(installer.InstallError): installer.verify_sdk_package(stage, manifest, 'a'*40)

    def test_archive_mode_lie_and_path_traversal_refuse(self):
        release = self.derive();archive = self.output/release['asset'];bad = self.root/'bad.tar.gz'
        with tarfile.open(archive, 'r:gz') as original, tarfile.open(bad, 'w:gz') as target:
            for member in original:
                if member.name.endswith('/node/bin/node'): member.mode = 0o644
                target.addfile(member, original.extractfile(member))
        stage = self.root/'stage';stage.mkdir()
        with self.assertRaisesRegex(installer.InstallError, 'mode differs'): installer.safe_extract(bad, stage)
        self.assertEqual(list(stage.iterdir()), [])

    def test_current_channel_selects_exact_mac_archive_and_library_without_relabeling_history(self):
        release = self.derive()
        source, linux = synthetic_inputs()
        source = replace(source, revision='a'*40, tree='b'*40)
        mac = self.validate_mac(source, '20261011')
        candidate = compose.compose_candidate(source, linux, mac)
        row = candidate.channel['platforms']['macos-arm64']
        self.assertEqual(row['source'], 'a'*40)
        self.assertEqual(row['tag'], 'checkpoint-20261011-aaaaaaaaaa-macos-arm64')
        self.assertEqual(row['archive']['sha256'], release['sha256'])
        self.assertEqual(row['installer']['sha256'], hashlib.sha256(source.mac_library).hexdigest())
        pins = {key: library.bootstrap_pin for key, library in candidate.libraries.items()}
        compose.bootstrap.validate_channel(candidate.channel, bootstrap_tag=candidate.bootstrap_tag, libraries=pins)
        product, identity = compose.bootstrap.prepare_install_identity(row, 'macos-arm64', installer, None)
        self.assertEqual(product['source_sha'], 'a'*40)
        self.assertEqual(identity['sha256'], release['sha256'])
        # A Linux-only assembly continues to preserve the explicitly historical Mac row.
        legacy = compose.compose_candidate(source, linux)
        self.assertEqual(legacy.channel['platforms']['macos-arm64']['source'], source.base_channel['platforms']['macos-arm64']['source'])
        self.assertNotEqual(legacy.bootstrap_tag, candidate.bootstrap_tag)
        retag = replace(mac, tag='checkpoint-20261012-aaaaaaaaaa-macos-arm64')
        self.assertNotEqual(compose.compose_candidate(source, linux, retag).bootstrap_tag, candidate.bootstrap_tag)
        self.assertIn('not established', row['known_failures'][0])
        # Emit and independently inventory both selected archives and their manifests.
        archive = self.root/linux.archive.name;archive.write_bytes(b'explicit ordinary Linux transport fixture')
        body = archive.read_bytes();sha = hashlib.sha256(body).hexdigest()
        manifest = dict(linux.manifest, sha256=sha, size_bytes=len(body))
        raw = json.dumps(manifest).encode();manifest_sha = hashlib.sha256(raw).hexdigest()
        linux = replace(linux, directory=self.root, archive=archive, archive_sha256=sha, archive_bytes=len(body),
                        manifest=manifest, manifest_bytes=raw, manifest_sha256=manifest_sha,
                        installer=compose.pin_linux_installer(linux.installer_source_template, linux.tag, manifest_sha))
        (self.root/(archive.name+'.sha256')).write_text(sha+'  '+archive.name+'\n')
        receipt = compose.prepare_output(compose.compose_candidate(source, linux, mac), self.root/'channel-output')
        with self.assertRaisesRegex(compose.CandidateError, 'Mac package input'):
            compose.prepare_output(compose.compose_candidate(source, linux, mac), self.output/'nested-channel')
        self.assertFalse((self.output/'nested-channel').exists())
        self.assertEqual(receipt['macos_identity']['archive']['sha256'], release['sha256'])
        self.assertEqual(receipt['assets'][release['asset']]['sha256'], release['sha256'])
        self.assertFalse(receipt['native_acceptance_claimed'])
        self.assertFalse(receipt['published'])

    def test_mac_channel_refuses_source_hash_length_installer_and_calendar_drift(self):
        release = self.derive()
        source, _ = synthetic_inputs();source = replace(source, revision='a'*40, tree='b'*40)
        with self.assertRaises(compose.CandidateError): self.validate_mac(source, '20260231')
        path = self.output/'release-manifest.json';original = json.loads(path.read_text())
        for field, value in [('source_sha', 'f'*40), ('sha256', '0'*64), ('size_bytes', release['size_bytes']+1)]:
            bad = dict(original);bad[field] = value;write_json(path, bad)
            with self.assertRaises(compose.CandidateError): self.validate_mac(source, '20261011')
        write_json(path, original)
        (self.output/'install-macos-arm64.py').write_text('not selected source')
        with self.assertRaisesRegex(compose.CandidateError, 'installer differs'): self.validate_mac(source, '20261011')

    def test_aliases_remain_resolvable_during_interrupted_layout_publication(self):
        release = self.derive();legacy = legacy_fixtures.Tests().archive(self.root)
        prefix = self.root/'interrupted-home/.local'
        entry = {'version': '0.0.0', 'source_sha': 'a'*40, 'tag': 'checkpoint-20261011-aaaaaaaaaa-macos-arm64', 'asset': release['asset']}
        with patch.object(installer, 'check_startup'):
            installer.install(prefix, entry, {'sha256': installer._file_sha256(legacy)}, legacy)
            old = (prefix/'bin/nudox').resolve()
            replace_link = installer._replace_symlink
            def interrupt(link, target):
                self.assertTrue(all((prefix/'bin'/name).resolve(strict=True).is_file() for name in installer.command_links(prefix/'lib/nudox')))
                if link.name == 'current': raise OSError('unit interruption before pointer replacement')
                return replace_link(link, target)
            entry = dict(entry, tag='checkpoint-20261012-aaaaaaaaaa-macos-arm64')
            with patch.object(installer, '_replace_symlink', side_effect=interrupt):
                with self.assertRaises(OSError): installer.install(prefix, entry, {'sha256': release['sha256']}, self.output/release['asset'])
            self.assertEqual((prefix/'bin/nudox').resolve(), old)
            self.assertTrue(installer.reuse_existing_install(prefix, entry, {'sha256': release['sha256']}))
            self.assertEqual((prefix/'bin/nudox').resolve().parent.name, 'MacOS')
            (prefix/'lib/nudox/current/bin').unlink()
            (prefix/'lib/nudox/current/bin').symlink_to('/usr/bin')
            with self.assertRaises(installer.InstallError): installer.reuse_existing_install(prefix, entry, {'sha256': release['sha256']})

    def test_mac_archive_replacement_between_hash_and_parse_refuses(self):
        release = self.derive();source, _ = synthetic_inputs();source = replace(source, revision='a'*40, tree='b'*40)
        archive = self.output/release['asset'];original = installer.safe_extract
        def swap(path, stage, **kwargs):
            self.assertIsNotNone(kwargs.get('archive_file'))
            archive.rename(self.output/'original-admitted.tar.gz')
            archive.write_bytes(b'replacement must never be parsed')
            return original(path, stage, **kwargs)
        with patch.object(installer, 'safe_extract', side_effect=swap):
            with self.assertRaisesRegex(compose.CandidateError, 'identity changed'): self.validate_mac(source, '20261011')

    def test_release_config_requires_actual_sdk_only_path(self):
        self.assertFalse(macos_release.sdk_cli_archive({}))
        self.assertTrue(macos_release.sdk_cli_archive({'sdk_only': True, 'sdk_cli_archive': True}))
        for config in ({'sdk_cli_archive': True}, {'sdk_only': True, 'sdk_cli_archive': 'true'}):
            with self.assertRaises(ValueError): macos_release.sdk_cli_archive(config)


if __name__ == '__main__': unittest.main()
