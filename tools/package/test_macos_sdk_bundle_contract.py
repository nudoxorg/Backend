#!/usr/bin/env python3
"""SDK packaging data/lifecycle contracts; ordinary fixtures are not native SDK proof."""
import importlib.util
import json
import plistlib
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

def module(filename, name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    value = importlib.util.module_from_spec(spec)
    sys.modules[name] = value
    spec.loader.exec_module(value)
    return value

bundle = module('macos-investor-bundle.py', 'sdk_bundle_contract')
collector = module('collect-macos-macho-relocation.py', 'sdk_collector_contract')
stager = module('build_macos_typescript_sdk.py', 'sdk_stager_contract')

class SdkOnlyContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.payload = self.root / 'payload'
        package = 'typescript/node_modules/typescript/'
        files = {'typescript/node/bin/node': b'unit fixture, not a runtime', 'typescript/node/LICENSE': b'Node notice',
                 package+'package.json': b'{"name":"typescript","version":"5.9.3"}',
                 package+'bin/tsc': b'unit compiler entry', package+'lib/typescript.js': b'unit API entry',
                 package+'LICENSE.txt': b'TypeScript notice', package+'ThirdPartyNoticeText.txt': b'notices'}
        for name, value in files.items():
            p = self.payload / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_bytes(value)
        (self.payload/'typescript/node/bin/node').chmod(0o755)
        self.source = dict(git_revision='a'*40, git_tree='b'*40, cargo_lock_sha256='c'*64, working_tree='clean')
        self.target = 'aarch64-apple-darwin'
        self.receipt = dict(schema=1, source=self.source, target=self.target,
                            files={name:bundle.sha256(self.payload/name) for name in files},
                            tools={'node':{'version':'v24.18.0','sha256':bundle.sha256(self.payload/'typescript/node/bin/node')},'typescript':{'version':'5.9.3'}},
                            notices={'node':'typescript/node/LICENSE','typescript':package+'LICENSE.txt','typescript_third_party':package+'ThirdPartyNoticeText.txt'})

    def validate(self):
        return bundle.validate_sdk_helper_payload(self.receipt, self.payload, self.source, self.target)

    def test_sdk_admits_only_complete_exact_source_and_target(self):
        self.validate()
        self.receipt['source'] = {k:v for k,v in self.source.items() if k in ['git_revision','git_tree']}
        with self.assertRaisesRegex(bundle.PackageError,'complete clean'):
            self.validate()
        self.receipt['source'] = self.source
        self.receipt['target'] = 'x86_64-apple-darwin'
        with self.assertRaisesRegex(bundle.PackageError,'target'):
            self.validate()

    def test_sdk_detects_changed_package_and_missing_license(self):
        p = self.payload/'typescript/node_modules/typescript/lib/typescript.js'
        p.write_bytes(b'changed')
        with self.assertRaisesRegex(bundle.PackageError,'hash receipt'):
            self.validate()
        self.receipt['files'][str(p.relative_to(self.payload))] = bundle.sha256(p)
        (self.payload/'typescript/node/LICENSE').unlink()
        with self.assertRaisesRegex(bundle.PackageError,'hash receipt'):
            self.validate()

    def test_sdk_rejects_unreceipted_directory_and_symlink(self):
        p = self.payload/'extra';p.mkdir()
        with self.assertRaisesRegex(bundle.PackageError,'unreceipted directory'):
            self.validate()
        p.rmdir();p.symlink_to(self.payload/'typescript/node/LICENSE')
        with self.assertRaisesRegex(bundle.PackageError,'link/special'):
            self.validate()

    def test_sdk_rejects_fabricated_legacy_helpers_and_wrong_node_identity(self):
        self.receipt['tools']['go_oracle'] = {'version':'unit'}
        with self.assertRaisesRegex(bundle.PackageError,'exactly Node'):
            self.validate()
        del self.receipt['tools']['go_oracle'];self.receipt['tools']['node']['sha256']='0'*64
        with self.assertRaisesRegex(bundle.PackageError,'tool identity'):
            self.validate()

    def test_sdk_file_and_package_byte_limits_precede_hashing(self):
        p = self.payload/'typescript/node_modules/typescript/lib/typescript.js'
        with p.open('wb') as f:f.truncate(96*1024**2+1)
        with self.assertRaisesRegex(bundle.PackageError,'bounded identity admission'):
            self.validate()

    def test_relocation_receipt_keeps_original_and_binds_actual_changed_node(self):
        original = self.root/'source-receipt.json';original.write_text(json.dumps(self.receipt))
        original_bytes = original.read_bytes()
        (self.payload/'typescript/node/bin/node').write_bytes(b'relocated unit fixture')
        provenance = self.root/'provenance';provenance.mkdir()
        receipt, path = bundle.write_sdk_bundle_receipt(self.payload, provenance, self.source, self.target, self.receipt, original)
        self.assertEqual(original.read_bytes(), original_bytes)
        self.assertEqual(receipt['source_receipt_sha256'], bundle.sha256(original))
        self.assertEqual(receipt['tools']['node']['sha256'], bundle.sha256(self.payload/'typescript/node/bin/node'))
        self.assertNotEqual(receipt['tools']['node']['sha256'], self.receipt['tools']['node']['sha256'])
        self.assertEqual(json.loads(path.read_text())['source'], self.source)

    def test_runtime_probe_selects_absolute_node_and_module_without_overrides(self):
        seen = []
        def run(argv, **kwargs):
            seen.append((argv,kwargs))
            self.assertEqual(set(kwargs['env']), {'PATH','HOME','TMPDIR'})
            self.assertEqual(argv[0], str(self.payload/'typescript/node/bin/node'))
            output = b'v24.18.0\n' if argv[1:] == ['--version'] else b'5.9.3'
            return subprocess.CompletedProcess(argv,0,output,b'')
        with patch.object(bundle.subprocess,'run',side_effect=run):
            result=bundle.verify_sdk_runtime(self.payload,self.receipt)
        self.assertEqual(result['typescript']['version'],'5.9.3')
        self.assertEqual(seen[1][0][-1], str(self.payload/'typescript/node_modules/typescript'))
        with patch.object(bundle.subprocess,'run',return_value=subprocess.CompletedProcess([],0,b'wrong',b'')):
            with self.assertRaisesRegex(bundle.PackageError,'version probe'):
                bundle.verify_sdk_runtime(self.payload,self.receipt)

    def test_sdk_launcher_propagates_literal_arguments_without_injected_tool_paths(self):
        macos=self.root/'Nudox.app/Contents/MacOS';macos.mkdir(parents=True)
        desktop=macos/'backend-desktop';desktop.write_text(f'#!{sys.executable}\nimport os,json,sys\nprint(json.dumps(dict(args=sys.argv[1:],overrides={{k:v for k,v in os.environ.items() if k.startswith(("NUDOX_","BACKEND_"))}})))\n');desktop.chmod(0o755)
        bundle.write_application_launcher(macos,sdk_only=True)
        observed=json.loads(subprocess.check_output([str(macos/'Nudox'),'space argument','$(literal)'],env={'PATH':'/usr/bin:/bin'}))
        self.assertEqual(observed,dict(args=['space argument','$(literal)'],overrides={}))

    def test_sdk_collector_declares_four_application_processes_and_only_real_sdk_origin(self):
        artifact=self.root/'images';artifact.mkdir()
        for name in bundle.EXECUTABLES:(artifact/name).write_bytes(b'image')
        app_receipt=self.root/'app.json';app_receipt.write_text('{}')
        helper_receipt=self.root/'helpers.json';helper_receipt.write_text(json.dumps(self.receipt))
        args=types.SimpleNamespace(artifact_dir=artifact,expected_runner_sha256='d'*64,sdk_only=True,helpers_dir=self.payload,source_root=self.root)
        with patch.object(collector.bundle,'validate_app_build',return_value=({}, {n:artifact/n for n in bundle.EXECUTABLES})):
            origins, roots, processes=collector.source_origin_records(args,{'target':{'triple':self.target,'architecture':'arm64'}},self.source,{'application':app_receipt,'helpers':helper_receipt})
        self.assertEqual(set(origins), {'application','helpers'})
        self.assertIn('Contents/MacOS/backend-cli',processes)
        self.assertEqual(len(processes),5)
        self.assertEqual(origins['helpers']['process_roots'], {'typescript/node/bin/node':'Contents/Resources/Helpers/typescript/node/bin/node'})

    def test_sdk_assembly_emits_source_bound_inventory_and_no_legacy_claims(self):
        source_root=self.root/'source';source_root.mkdir()
        plist=source_root/'apps/desktop/macos/Info.plist';plist.parent.mkdir(parents=True)
        plist.write_bytes(plistlib.dumps(dict(CFBundleExecutable='Nudox',CFBundlePackageType='APPL',CFBundleIdentifier='dev.nudox.desktop')))
        (source_root/'Cargo.toml').write_text('[workspace.package]\nversion="0.1.0"\n')
        fonts=source_root/'apps/facet/resources/fonts';fonts.mkdir(parents=True)
        for name in bundle.FONT_LICENSES:(fonts/name).write_text('unit font notice')
        artifacts=self.root/'images';artifacts.mkdir()
        for name in bundle.EXECUTABLES:
            p=artifacts/name;p.write_bytes(b'unit image');p.chmod(0o755)
        app_path=self.root/'application-receipt.json';app_path.write_text('{}')
        helper_path=self.root/'helper-receipt.json';helper_path.write_text(json.dumps(self.receipt))
        original_helper=helper_path.read_bytes()
        app=dict(profile='release',command=['unit-build'],cargo_runner_before={},cargo_runner_unchanged=True,cargo_provenance={})
        output=self.root/'output'
        argv=['bundle','--sdk-only','--source-root',str(source_root),'--expected-revision','a'*40,'--expected-tree','b'*40,
              '--expected-runner-sha256','d'*64,'--artifact-dir',str(artifacts),'--build-receipt',str(app_path),
              '--helpers-dir',str(self.payload),'--helpers-receipt',str(helper_path),'--output-dir',str(output)]
        def run(command,**kwargs):
            if command[0]=='plutil':return 'OK'
            self.assertEqual(command[0],'ditto');Path(command[-1]).write_bytes(b'unit zip');return ''
        def inspect(staging,arch,minimum,required):
            self.assertEqual(required, {f'Contents/MacOS/{n}' for n in bundle.EXECUTABLES}|{'Contents/Resources/Helpers/typescript/node/bin/node'})
            return []
        with patch.object(sys,'argv',argv), patch.object(bundle,'macos_tools'), patch.object(bundle.platform,'machine',return_value='arm64'), \
             patch.object(bundle,'validate_source',return_value=self.source), patch.object(bundle,'validate_app_build',return_value=(app,{n:artifacts/n for n in bundle.EXECUTABLES})), \
             patch.object(bundle,'validate_roslyn_build') as roslyn, patch.object(bundle,'validate_dotnet_runtime_receipt') as dotnet, \
             patch.object(bundle,'run',side_effect=run), patch.object(bundle,'inspect_macho_tree',side_effect=inspect), \
             patch.object(bundle,'inspect_signature',return_value='unsigned'), patch.object(bundle,'verify_sdk_runtime',return_value={'unit_probe':True}):
            self.assertEqual(bundle.main(),0);roslyn.assert_not_called();dotnet.assert_not_called()
        contents=output/'Nudox.app/Contents';manifest=json.loads((contents/'Resources/build-manifest.json').read_text())
        emitted=json.loads((contents/'Resources/Build Evidence/compiler-helpers-receipt.json').read_text())
        self.assertEqual(emitted['source'],manifest['source'])
        self.assertEqual(emitted['files'],manifest['compiler_helpers']['files'])
        self.assertEqual(emitted['tools'],manifest['compiler_helpers']['tools'])
        self.assertEqual((contents/'Resources/Build Evidence/compiler-helpers-source-receipt.json').read_bytes(),original_helper)
        self.assertNotIn('roslyn_build',manifest);self.assertNotIn('dotnet_runtime',manifest)
        self.assertFalse((contents/'Resources/dotnet').exists())
        self.assertIn('Contents/Resources/Build Evidence/compiler-helpers-receipt.json',manifest['files'])
        self.assertEqual(manifest['files']['Contents/MacOS/backend-cli']['sha256'],bundle.sha256(contents/'MacOS/backend-cli'))
        self.assertNotIn('NUDOX_', (contents/'MacOS/Nudox').read_text())

    def test_staging_copies_complete_package_and_preserves_explicit_source(self):
        node=self.payload/'typescript/node/bin/node';license=self.payload/'typescript/node/LICENSE';package=self.payload/'typescript/node_modules/typescript'
        with patch.object(stager.bundle,'validate_source',return_value=self.source):
            result=stager.stage(self.root,'a'*40,'b'*40,self.target,node,'v24.18.0',bundle.sha256(node),license,bundle.sha256(license),package,'5.9.3',self.root/'staged')
        actual=json.loads(Path(result['receipt']).read_text())
        self.assertEqual(actual['source'],self.source)
        self.assertEqual(actual['files'],self.receipt['files'])
        self.assertEqual(actual['runtime_probe_status'],'pending relocated bundle producer Node/Compiler API probes')
        with patch.object(stager.bundle,'validate_source',return_value=self.source):
            with self.assertRaisesRegex(stager.bundle.PackageError,'content pin'):
                stager.stage(self.root,'a'*40,'b'*40,self.target,node,'v24.18.0','0'*64,license,bundle.sha256(license),package,'5.9.3',self.root/'bad')
        self.assertFalse((self.root/'bad').exists())

if __name__=='__main__':unittest.main()
