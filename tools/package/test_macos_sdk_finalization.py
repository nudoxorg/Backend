"""Ordinary signing-mutation fixtures; these are not native SDK/release evidence."""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import unittest
from unittest.mock import patch

import finalize_macos_typescript_sdk as finalizer
import macos_release as release
import test_macos_sdk_bundle_contract as fixtures
import macho_signing_identity as identity
from test_macho_signing_identity import fixture, remove_signature


class SdkFinalizationContracts(unittest.TestCase):
    def setUp(self):
        fixtures.SdkOnlyContracts.setUp(self)
        tool = self.root / 'selected-codesign'
        tool.write_bytes(b'unit selected signing tool')
        for mock in (patch.object(identity, 'CODESIGN', tool),
                     patch.object(finalizer.bundle, 'bounded_sdk_probe', side_effect=remove_signature)):
            mock.start(); self.addCleanup(mock.stop)

    def admit(self, app, evidence):
        return finalizer.admit(app, evidence, self.output, self.selected_receipt_sha256, self.selected_manifest_sha256)

    def assembly(self):
        self.output = self.root / 'release'
        app = self.output / 'package/Nudox.app'
        payload = app / 'Contents/Resources/Helpers'
        shutil.copytree(self.payload, payload)
        node = payload / 'typescript/node/bin/node'
        node.write_bytes(fixture(code=b'original Node code'))
        original = copy.deepcopy(self.receipt)
        original['files']['typescript/node/bin/node'] = fixtures.bundle.sha256(node)
        original['tools']['node']['sha256'] = fixtures.bundle.sha256(node)
        provenance = app / 'Contents/Resources/Build Evidence'
        provenance.mkdir()
        origin = app / finalizer.ORIGIN
        origin.write_text(json.dumps(original))
        self.selected_receipt = self.root / 'selected-source-receipt.json'
        shutil.copyfile(origin, self.selected_receipt)
        self.selected_receipt_sha256 = fixtures.bundle.sha256(self.selected_receipt)
        receipt, receipt_path = fixtures.bundle.write_sdk_bundle_receipt(payload, provenance, self.source, self.target, original, origin)
        for name in fixtures.bundle.EXECUTABLES:
            image = app / 'Contents/MacOS' / name
            image.parent.mkdir(exist_ok=True)
            image.write_bytes(fixture(code=name.encode()))
        paths = {f'Contents/MacOS/{name}' for name in fixtures.bundle.EXECUTABLES}
        paths.add('Contents/Resources/Helpers/typescript/node/bin/node')
        executables={name:fixtures.bundle.sha256(app/'Contents/MacOS'/name) for name in fixtures.bundle.EXECUTABLES}
        build_receipt=self.output/'build/application-build-receipt.json'
        build_receipt.parent.mkdir()
        build_receipt.write_text(json.dumps({'source':self.source,'executables':executables}))
        evidence = {'schema':1, 'source':self.source,
                    'target':{'triple':self.target,'architecture':'arm64'},
                    'bundle':{'minimum_macos':'14.4','signature':'unsigned'},
                    'application_build':{'executables':executables,'receipt_sha256':fixtures.bundle.sha256(build_receipt)},
                    'compiler_helpers':{'assembly_mode':'typescript-sdk-only', 'receipt_sha256':fixtures.bundle.sha256(receipt_path),
                        'files':receipt['files'],'tools':receipt['tools'],'source_receipt_sha256':fixtures.bundle.sha256(origin)},
                    'macho_images':[{'path':name} for name in sorted(paths)],
                    'files':fixtures.bundle.file_inventory(app)}
        (app/finalizer.MANIFEST).write_text(json.dumps(evidence))
        self.selected_manifest_sha256=fixtures.bundle.sha256(app/finalizer.MANIFEST)
        (app.parent/'Nudox-macOS.receipt.json').write_text(json.dumps({'schema':1,'application_source':self.source,'bundle_manifest_sha256':self.selected_manifest_sha256}))
        return app, evidence

    def inspect(self, app, arch, minimum, required):
        self.assertEqual(arch,'arm64')
        self.assertEqual(minimum,'14.4')
        return [{'path':name,'sha256':fixtures.bundle.sha256(app/name),'signature':'signed'} for name in sorted(required)]

    def test_signed_node_receipt_and_inventory_refresh_preserve_compiler_provenance(self):
        app, evidence = self.assembly()
        original_manifest = (app/finalizer.MANIFEST).read_bytes()
        original_receipt = (app/finalizer.RECEIPT).read_bytes()
        original_origin = (app/finalizer.ORIGIN).read_bytes()
        precursor = self.admit(app,evidence)
        node = app/'Contents/Resources/Helpers/typescript/node/bin/node'
        node.write_bytes(fixture(b'changed unit signature' * 512, code=b'original Node code'))
        signature = app/'Contents/_CodeSignature/CodeResources'
        signature.parent.mkdir(); signature.write_bytes(b'unit previous seal')
        with patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime',return_value={'unit_post_sign_probe':True}) as probe:
            result = finalizer.refresh(app,evidence,precursor)
        emitted = json.loads((app/finalizer.RECEIPT).read_text())
        self.assertEqual(emitted['files']['typescript/node/bin/node'],fixtures.bundle.sha256(node))
        self.assertEqual(emitted['tools'],result['compiler_helpers']['tools'])
        self.assertEqual(emitted['files'],result['compiler_helpers']['files'])
        self.assertNotEqual(emitted['tools']['node']['sha256'],evidence['compiler_helpers']['tools']['node']['sha256'])
        self.assertEqual(result['application_build'],evidence['application_build'])
        self.assertEqual((self.output/'sdk-assembly-evidence/build-manifest.json').read_bytes(),original_manifest)
        self.assertEqual((self.output/'sdk-assembly-evidence/compiler-helpers-receipt.json').read_bytes(),original_receipt)
        self.assertEqual((app/finalizer.ORIGIN).read_bytes(),original_origin)
        self.assertNotIn(finalizer.MANIFEST,result['files'])
        self.assertNotIn('Contents/_CodeSignature/CodeResources',result['files'])
        probe.assert_called_once()

    def test_changed_assembly_is_rejected_before_signing(self):
        app,evidence=self.assembly()
        (app/'Contents/MacOS/backend-cli').write_bytes(b'changed')
        with self.assertRaisesRegex(ValueError,'inventory changed'):
            self.admit(app,evidence)
        self.assertFalse((self.output/'sdk-assembly-evidence').exists())

    def test_signing_must_not_change_package_before_native_probe(self):
        app,evidence=self.assembly()
        precursor=self.admit(app,evidence)
        (app/'Contents/Resources/Helpers/typescript/node_modules/typescript/lib/typescript.js').write_bytes(b'changed')
        with patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
            with self.assertRaisesRegex(ValueError,'package/notices changed'):
                finalizer.refresh(app,evidence,precursor)
            probe.assert_not_called()

    def test_unsigned_inner_image_blocks_probe_and_outer_seal(self):
        app,evidence=self.assembly();precursor=self.admit(app,evidence)
        with patch.object(finalizer.bundle,'inspect_macho_tree',return_value=[{'path':r['path'],'signature':'invalidated'} for r in evidence['macho_images']]), patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
            with self.assertRaisesRegex(ValueError,'set/signatures'):
                finalizer.refresh(app,evidence,precursor)
            probe.assert_not_called()

    def test_native_probe_failure_leaves_no_refreshed_manifest(self):
        app,evidence=self.assembly();precursor=self.admit(app,evidence)
        original=(app/finalizer.MANIFEST).read_bytes()
        with patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime',side_effect=finalizer.bundle.PackageError('unit native failure')):
            with self.assertRaisesRegex(finalizer.bundle.PackageError,'unit native failure'):
                finalizer.refresh(app,evidence,precursor)
        self.assertEqual((app/finalizer.MANIFEST).read_bytes(),original)

    def test_release_refresh_happens_before_outer_seal_without_later_manifest_write(self):
        self.source['cargo_lock_sha256']=__import__('hashlib').sha256(b'unit lock').hexdigest()
        app,evidence=self.assembly()
        source=self.root/'source';source.mkdir();(source/'Cargo.toml').write_text('[workspace.package]\nversion="0.2.0"\n')
        (source/'Cargo.lock').write_bytes(b'unit lock')
        config={'source_root':str(source),'output_dir':str(self.output),'expected_revision':self.source['git_revision'],
                'expected_tree':self.source['git_tree'],'minimum_os':'14.4','signing_identity':'unit Developer ID','notary_profile':'unit-profile','sdk_only':True,
                'helpers_receipt':str(self.selected_receipt)}
        outer_manifest=[]
        def run(command,**kwargs):
            if Path(command[0]).name=='codesign':
                self.assertEqual(command[0],'/usr/bin/codesign')
            if command[0]=='/usr/bin/codesign' and '--sign' in command:
                path=Path(command[-1])
                if path.is_file():
                    code = b'original Node code' if path.name == 'node' else path.name.encode()
                    path.write_bytes(fixture(b'changed unit signature' * 512, code=code))
                else:
                    refreshed=json.loads((app/finalizer.MANIFEST).read_text())
                    node=app/'Contents/Resources/Helpers/typescript/node/bin/node'
                    self.assertEqual(refreshed['compiler_helpers']['tools']['node']['sha256'],fixtures.bundle.sha256(node))
                    for record in refreshed['macho_images']:
                        self.assertEqual(record['sha256'],fixtures.bundle.sha256(app/record['path']))
                    outer_manifest.append((app/finalizer.MANIFEST).read_bytes())
                    sidecar=app/'Contents/_CodeSignature/CodeResources';sidecar.parent.mkdir();sidecar.write_bytes(b'unit outer seal')
            if command[0]=='ditto':Path(command[-1]).write_bytes(b'unit archive')
            return subprocess.CompletedProcess(command,0,'{"status":"Accepted"}' if command[:3]==['xcrun','notarytool','submit'] else '')
        with patch.dict(os.environ,{'PATH':'/hostile/shadow/bin'}), patch.object(release,'preflight',return_value={'ready':True}), patch.object(release,'run',side_effect=run), patch.object(release.subprocess,'check_output',return_value='EXECUTE'), patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime',return_value={'unit_post_sign_probe':True}):
            release.finalize(config)
        self.assertEqual(len(outer_manifest),1)
        self.assertEqual((app/finalizer.MANIFEST).read_bytes(),outer_manifest[0])
        manifest=json.loads((self.output/'candidate/release-manifest.json').read_text())
        self.assertEqual(manifest['build_manifest_sha256'],fixtures.bundle.sha256(app/finalizer.MANIFEST))

    def test_sdk_prepare_omits_legacy_inputs_and_requires_post_sign_probes(self):
        source=self.root/'source';source.mkdir()
        config={'source_root':str(source),'output_dir':str(self.root/'prepared'),'expected_revision':'a'*40,
                'expected_tree':'b'*40,'expected_runner_sha256':'c'*64,'minimum_os':'14.4','expected_icon_sha256':'d'*64,'sdk_only':True}
        for key in release.INPUTS-release.LEGACY_INPUTS:config[key]='/unit/'+key
        with patch.object(release,'preflight',return_value={'ready':True}), patch.object(release,'run') as run, patch.object(release,'finalize'):
            release.prepare(config)
        arguments=run.call_args_list[1].args[0]
        self.assertIn('--sdk-only',arguments);self.assertIn('--defer-sdk-runtime-probes',arguments)
        self.assertFalse(any('--'+key.replace('_','-') in arguments for key in release.LEGACY_INPUTS))
        with self.assertRaisesRegex(ValueError,'boolean'):release.sdk_only({'sdk_only':'true'})
        with self.assertRaisesRegex(ValueError,'legacy helper'):release.sdk_only({'sdk_only':True,'dotnet_root':'/unit/dotnet'})
        self.assertEqual(release.release_inputs({}),release.INPUTS)

    def test_sdk_release_bounds_manifest_before_json_parsing_or_signing(self):
        app,evidence=self.assembly()
        with (app/finalizer.MANIFEST).open('wb') as stream:stream.truncate(16*1024**2+1)
        config={'source_root':str(self.root),'output_dir':str(self.output),'sdk_only':True}
        with patch.object(release,'preflight',return_value={'ready':True}), patch.object(release.json,'loads') as parse, patch.object(release,'run') as run:
            with self.assertRaisesRegex(ValueError,'regular file under its byte bound'):
                release.finalize(config)
            parse.assert_not_called();run.assert_not_called()

    def test_native_probe_cannot_leave_stale_application_image_hashes(self):
        app,evidence=self.assembly();precursor=self.admit(app,evidence)
        original=(app/finalizer.MANIFEST).read_bytes()
        original_receipt=(app/finalizer.RECEIPT).read_bytes()
        def probe(*args):
            image=app/'Contents/MacOS/backend-cli'
            image.write_bytes(image.read_bytes()+b'unit concurrent mutation')
            return {'unit_probe':True}
        with patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime',side_effect=probe):
            with self.assertRaisesRegex(ValueError,'files changed during native probes'):
                finalizer.refresh(app,evidence,precursor)
        self.assertEqual((app/finalizer.MANIFEST).read_bytes(),original)
        self.assertEqual((app/finalizer.RECEIPT).read_bytes(),original_receipt)

    def test_unexpected_nested_signature_sidecar_is_not_ignored(self):
        app,evidence=self.assembly();precursor=self.admit(app,evidence)
        sidecar=app/'Contents/Resources/unexpected/_CodeSignature/CodeResources'
        sidecar.parent.mkdir(parents=True);sidecar.write_bytes(b'not an admitted outer seal')
        with patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
            with self.assertRaisesRegex(ValueError,'non-code SDK assembly bytes'):
                finalizer.refresh(app,evidence,precursor)
            probe.assert_not_called()

    def test_signed_substitution_of_node_or_application_is_rejected_before_probe(self):
        for name in ('Contents/Resources/Helpers/typescript/node/bin/node', 'Contents/MacOS/backend-cli'):
            with self.subTest(image=name):
                # Separate assembly preserves each admission's private precursor.
                if hasattr(self, 'output'):
                    shutil.rmtree(self.output)
                app,evidence=self.assembly();precursor=self.admit(app,evidence)
                (app/name).write_bytes(fixture(b'signed replacement', code=b'substitute code'))
                with patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
                    with self.assertRaisesRegex(ValueError,'code differs'):
                        finalizer.refresh(app,evidence,precursor)
                    probe.assert_not_called()

    def test_self_consistent_replaced_origin_cannot_change_external_selection(self):
        app,evidence=self.assembly()
        origin=app/finalizer.ORIGIN
        origin.write_bytes(origin.read_bytes()+b'\n')
        new_pin=fixtures.bundle.sha256(origin)
        receipt_path=app/finalizer.RECEIPT
        receipt=json.loads(receipt_path.read_text());receipt['source_receipt_sha256']=new_pin
        receipt_path.write_text(json.dumps(receipt))
        evidence['compiler_helpers']['source_receipt_sha256']=new_pin
        evidence['compiler_helpers']['receipt_sha256']=fixtures.bundle.sha256(receipt_path)
        evidence['files']=fixtures.bundle.file_inventory(app);evidence['files'].pop(finalizer.MANIFEST)
        (app/finalizer.MANIFEST).write_text(json.dumps(evidence))
        # Isolate the helper-origin selector; the manifest selector has its own test.
        self.selected_manifest_sha256=fixtures.bundle.sha256(app/finalizer.MANIFEST)
        with self.assertRaisesRegex(ValueError,'manifest/origin binding'):
            self.admit(app,evidence)
        self.assertFalse((self.output/'sdk-assembly-evidence').exists())

    def test_coherent_pre_sign_executable_manifest_replacement_cannot_change_external_package_pin(self):
        app,evidence=self.assembly()
        image=app/'Contents/MacOS/backend-cli';image.write_bytes(fixture(code=b'replaced before signing'))
        evidence['files']=fixtures.bundle.file_inventory(app);evidence['files'].pop(finalizer.MANIFEST)
        (app/finalizer.MANIFEST).write_text(json.dumps(evidence))
        with self.assertRaisesRegex(ValueError,'selected external package receipt'):
            self.admit(app,evidence)
        self.assertFalse((self.output/'sdk-assembly-evidence').exists())

    def test_release_cross_checks_external_build_receipt_before_signing(self):
        self.source['cargo_lock_sha256']=__import__('hashlib').sha256(b'unit lock').hexdigest()
        app,evidence=self.assembly()
        source=self.root/'source';source.mkdir();(source/'Cargo.lock').write_bytes(b'unit lock')
        config={'source_root':str(source),'output_dir':str(self.output),'sdk_only':True,
                'expected_revision':self.source['git_revision'],'expected_tree':self.source['git_tree'],
                'minimum_os':'14.4','helpers_receipt':str(self.selected_receipt)}
        receipt=self.output/'build/application-build-receipt.json'
        value=json.loads(receipt.read_text());value['executables']['backend-cli']='0'*64
        receipt.write_text(json.dumps(value))
        with patch.object(release,'preflight',return_value={'ready':True}), patch.object(release,'run') as run:
            with self.assertRaisesRegex(ValueError,'external application build receipt'):
                release.finalize(config)
            run.assert_not_called()

    def test_mode_mutation_after_admission_is_rejected_before_probes(self):
        app,evidence=self.assembly();precursor=self.admit(app,evidence)
        (app/'Contents/Resources/Helpers/typescript/node/bin/node').chmod(0o777)
        with patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
            with self.assertRaisesRegex(finalizer.bundle.PackageError,'0755/0644'):
                finalizer.refresh(app,evidence,precursor)
            probe.assert_not_called()


if __name__=='__main__':unittest.main()
