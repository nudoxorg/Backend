"""Ordinary signing-mutation fixtures; these are not native SDK/release evidence."""
import copy
import json
from pathlib import Path
import shutil
import subprocess
import unittest
from unittest.mock import patch

import finalize_macos_typescript_sdk as finalizer
import macos_release as release
import test_macos_sdk_bundle_contract as fixtures


class SdkFinalizationContracts(unittest.TestCase):
    def setUp(self):
        fixtures.SdkOnlyContracts.setUp(self)

    def assembly(self):
        self.output = self.root / 'release'
        app = self.output / 'package/Nudox.app'
        payload = app / 'Contents/Resources/Helpers'
        shutil.copytree(self.payload, payload)
        node = payload / 'typescript/node/bin/node'
        node.write_bytes(b'\xcf\xfa\xed\xfeunit Node fixture')
        original = copy.deepcopy(self.receipt)
        original['files']['typescript/node/bin/node'] = fixtures.bundle.sha256(node)
        original['tools']['node']['sha256'] = fixtures.bundle.sha256(node)
        provenance = app / 'Contents/Resources/Build Evidence'
        provenance.mkdir()
        origin = app / finalizer.ORIGIN
        origin.write_text(json.dumps(original))
        receipt, receipt_path = fixtures.bundle.write_sdk_bundle_receipt(payload, provenance, self.source, self.target, original, origin)
        for name in fixtures.bundle.EXECUTABLES:
            image = app / 'Contents/MacOS' / name
            image.parent.mkdir(exist_ok=True)
            image.write_bytes(b'\xcf\xfa\xed\xfeunit application fixture')
        paths = {f'Contents/MacOS/{name}' for name in fixtures.bundle.EXECUTABLES}
        paths.add('Contents/Resources/Helpers/typescript/node/bin/node')
        evidence = {'schema':1, 'source':self.source,
                    'target':{'triple':self.target,'architecture':'arm64'},
                    'bundle':{'minimum_macos':'14.4','signature':'unsigned'},
                    'application_build':{'executables':{name:fixtures.bundle.sha256(app/'Contents/MacOS'/name) for name in fixtures.bundle.EXECUTABLES}},
                    'compiler_helpers':{'assembly_mode':'typescript-sdk-only', 'receipt_sha256':fixtures.bundle.sha256(receipt_path),
                        'files':receipt['files'],'tools':receipt['tools'],'source_receipt_sha256':fixtures.bundle.sha256(origin)},
                    'macho_images':[{'path':name} for name in sorted(paths)],
                    'files':fixtures.bundle.file_inventory(app)}
        (app/finalizer.MANIFEST).write_text(json.dumps(evidence))
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
        precursor = finalizer.admit(app,evidence,self.output)
        node = app/'Contents/Resources/Helpers/typescript/node/bin/node'
        node.write_bytes(node.read_bytes()+b'unit code signature')
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
            finalizer.admit(app,evidence,self.output)
        self.assertFalse((self.output/'sdk-assembly-evidence').exists())

    def test_signing_must_not_change_package_before_native_probe(self):
        app,evidence=self.assembly()
        precursor=finalizer.admit(app,evidence,self.output)
        (app/'Contents/Resources/Helpers/typescript/node_modules/typescript/lib/typescript.js').write_bytes(b'changed')
        with patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
            with self.assertRaisesRegex(ValueError,'package/notices changed'):
                finalizer.refresh(app,evidence,precursor)
            probe.assert_not_called()

    def test_unsigned_inner_image_blocks_probe_and_outer_seal(self):
        app,evidence=self.assembly();precursor=finalizer.admit(app,evidence,self.output)
        with patch.object(finalizer.bundle,'inspect_macho_tree',return_value=[{'path':r['path'],'signature':'invalidated'} for r in evidence['macho_images']]), patch.object(finalizer.bundle,'verify_sdk_runtime') as probe:
            with self.assertRaisesRegex(ValueError,'set/signatures'):
                finalizer.refresh(app,evidence,precursor)
            probe.assert_not_called()

    def test_native_probe_failure_leaves_no_refreshed_manifest(self):
        app,evidence=self.assembly();precursor=finalizer.admit(app,evidence,self.output)
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
                'expected_tree':self.source['git_tree'],'minimum_os':'14.4','signing_identity':'unit Developer ID','notary_profile':'unit-profile','sdk_only':True}
        outer_manifest=[]
        def run(command,**kwargs):
            if command[0]=='codesign' and '--sign' in command:
                path=Path(command[-1])
                if path.is_file():
                    path.write_bytes(path.read_bytes()+b'unit signature mutation')
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
        with patch.object(release,'preflight',return_value={'ready':True}), patch.object(release,'run',side_effect=run), patch.object(release.subprocess,'check_output',return_value='EXECUTE'), patch.object(finalizer.bundle,'inspect_macho_tree',side_effect=self.inspect), patch.object(finalizer.bundle,'verify_sdk_runtime',return_value={'unit_post_sign_probe':True}):
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


if __name__=='__main__':unittest.main()
