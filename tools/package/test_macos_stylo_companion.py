"""Finite custody negatives for the macOS Stylo companion."""
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import macos_stylo_companion as companion

class CompanionTests(unittest.TestCase):
    def test_system_loads_reject_relative_non_system_and_parent_paths(self):
        image = Path('/tmp/worker')
        self.assertEqual(companion.parse_system_loads(
            b'/tmp/worker:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 2.0.0)\n', image),
            ['/usr/lib/libSystem.B.dylib'])
        for name in ('@rpath/libbad.dylib', '@loader_path/libbad.dylib', '/opt/homebrew/lib/libbad.dylib', '/usr/lib/../tmp/libbad.dylib'):
            output = f'/tmp/worker:\n\t{name} (compatibility version 1.0.0, current version 2.0.0)\n'.encode()
            with self.subTest(name=name), self.assertRaises(ValueError):
                companion.parse_system_loads(output, image)

    def test_developer_requirement_is_fixed_and_team_input_is_validated(self):
        requirement = companion.developer_id_requirement('A1B2C3D4E5')
        self.assertTrue(requirement.startswith('=anchor apple generic and '))
        self.assertIn('1.2.840.113635.100.6.2.6', requirement)
        self.assertIn('1.2.840.113635.100.6.1.13', requirement)
        self.assertIn('subject.OU] = "A1B2C3D4E5"', requirement)
        with self.assertRaises(ValueError):
            companion.developer_id_requirement('A" or true')

    def test_artifact_rejects_symlink_and_oversize(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            real = root/'worker'
            real.write_bytes(b'worker')
            link = root/'link'
            link.symlink_to(real)
            with self.assertRaises(ValueError):
                companion.digest_regular(link)
            with self.assertRaises(ValueError):
                companion.digest_regular(real, 3)

    def test_copy_is_bounded_and_requires_admitted_content(self):
        import hashlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root/'worker'
            source.write_bytes(b'exact worker')
            digest = hashlib.sha256(source.read_bytes()).hexdigest()
            companion.copy_admitted(source, root/'copy', len(b'exact worker'), digest)
            self.assertEqual((root/'copy').read_bytes(), b'exact worker')
            with self.assertRaises(ValueError):
                companion.copy_admitted(source, root/'wrong', len(b'exact worker'), 'f'*64)
            self.assertFalse((root/'wrong').exists())
            with self.assertRaises(ValueError):
                companion.copy_admitted(source, root/'too-small', 3, digest)
            self.assertFalse((root/'too-small').exists())

    def test_team_and_loader_inputs_fail_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            worker = Path(directory)/'worker'
            worker.write_bytes(b'worker')
            worker.chmod(0o700)
            header = f'{worker}:\nMach header\ncolumns\nMH_MAGIC_64 ARM64 ALL 0x00 EXECUTE 19 100 0\n'.encode()
            loads = f'{worker}:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 2.0.0)\n'.encode()
            valid_loader = b'cmd LC_LOAD_DYLINKER\n cmdsize 32\n name /usr/lib/dyld (offset 12)\n'
            for extra in (b'cmd LC_RPATH\n', b'cmd LC_DYLD_ENVIRONMENT\n'):
                responses = [b'arm64\n', header, loads, valid_loader + extra]
                with self.subTest(extra=extra), patch.object(companion, 'run_tool', side_effect=responses), self.assertRaisesRegex(ValueError, 'dyld environment'):
                    companion.inspect_signed_worker(worker, 'aarch64-apple-darwin', 'A1B2C3D4E5')
            responses = [b'arm64\n', header, loads, valid_loader.replace(b'/usr/lib/dyld',b'/tmp/dyld')]
            with patch.object(companion, 'run_tool', side_effect=responses), self.assertRaisesRegex(ValueError, 'dynamic linker'):
                companion.inspect_signed_worker(worker, 'aarch64-apple-darwin', 'A1B2C3D4E5')
            responses = [b'arm64\n', header.replace(b'EXECUTE', b'DYLIB')]
            with patch.object(companion, 'run_tool', side_effect=responses), self.assertRaisesRegex(ValueError, 'Mach-O executable'):
                companion.inspect_signed_worker(worker, 'aarch64-apple-darwin', 'A1B2C3D4E5')
            responses = [b'arm64\n', header, loads, valid_loader, b'', b'TeamIdentifier=OTHERTEAM1\n']
            with patch.object(companion, 'run_tool', side_effect=responses), self.assertRaisesRegex(ValueError, 'team'):
                companion.inspect_signed_worker(worker, 'aarch64-apple-darwin', 'A1B2C3D4E5')

if __name__ == '__main__':
    unittest.main()

class ReceiptTests(unittest.TestCase):
    def test_actor_owner_and_signed_worker_are_all_bound(self):
        import hashlib
        import json
        import os
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root/'source'
            code = source/'tools/package'
            code.mkdir(parents=True)
            actor = code/'record_macos_stylo_build.py'
            checker = code/'macos_stylo_companion.py'
            actor.write_bytes(b'actor')
            checker.write_bytes(b'checker')
            output = root/'evidence'
            output.mkdir()
            target = root/'target'
            build = root/'build'
            cargo_home = root/'cargo-home'
            for path in (target,build,cargo_home): path.mkdir()
            tools = root/'tools'
            tools.mkdir()
            cargo = tools/'cargo'
            rustc = tools/'rustc'
            cargo.write_bytes(b'cargo')
            rustc.write_bytes(b'rustc')
            cargo.chmod(0o700)
            rustc.chmod(0o700)
            worker = output/'backend-stylo-worker'
            raw = output/'backend-stylo-worker.raw'
            owner = output/'backend-locald'
            observer = target/'aarch64-apple-darwin/release/examples/macos_stylo_identity_observer'
            observer.parent.mkdir(parents=True)
            for path in (worker,raw,owner,observer):
                path.write_bytes(path.name.encode())
                path.chmod(0o700)
            digest = lambda p: hashlib.sha256(p.read_bytes()).hexdigest()
            artifact = lambda p: {'path':str(p), 'bytes':p.stat().st_size, 'sha256':digest(p)}
            private = [str(source),str(cargo_home),str(target),str(build)]
            labels = ('source','cargo-home','target','build')
            rustflags = '\x1f'.join(f'--remap-path-prefix={p}=/nudox/{n}' for p,n in zip(private,labels))
            cflags = ' '.join(f'-{kind}-prefix-map={p}=/nudox/{n}' for p,n in zip(private,labels) for kind in ('ffile','fmacro'))
            env = {'PATH':str(tools)+':/usr/bin:/bin:/usr/sbin:/sbin','HOME':str(build),'TMPDIR':str(build),'LANG':'C','LC_ALL':'C','TZ':'UTC','RUSTC':str(rustc),'CARGO_HOME':str(cargo_home),'CARGO_TARGET_DIR':str(target),'CARGO_BUILD_BUILD_DIR':str(build),'CARGO_BUILD_TARGET':'aarch64-apple-darwin','CARGO_NET_OFFLINE':'true','CARGO_BUILD_JOBS':'2','CARGO_INCREMENTAL':'0','CARGO_CACHE_AUTO_CLEAN_FREQUENCY':'never','CARGO_TERM_COLOR':'never','RUSTC_WRAPPER':'','RUSTC_WORKSPACE_WRAPPER':'','RUSTFLAGS':'','CARGO_ENCODED_RUSTFLAGS':rustflags,'CFLAGS':cflags,'CXXFLAGS':cflags}
            commands = {'worker':['cargo','build','--release','--locked','--offline','-p','backend-frontend-stylo','--bin','backend-stylo-worker'], 'observer':['cargo','build','--release','--locked','--offline','-p','backend-compile','--example','macos_stylo_identity_observer'], 'owner':['cargo','build','--release','--locked','--offline','-p','backend-locald','--bin','backend-locald']}
            team='A1B2C3D4E5'
            anchor='a'*64
            src={'git_revision':'1'*40,'git_tree':'2'*40,'cargo_lock_sha256':'3'*64}
            worker_record={**artifact(worker),'architecture':'arm64','team_id':team,'system_loads':['/usr/lib/libSystem.B.dylib'],'signature_requirement':companion.developer_id_requirement(team)}
            receipt={'schema':'nudox.macos-stylo-bound-owner.v1','source':{'commit':src['git_revision'],'tree':src['git_tree'],'cargo_lock_sha256':src['cargo_lock_sha256'],'clean_before':True,'clean_after':True},'target':'aarch64-apple-darwin','exit':0,'limits':{'timeout_seconds':1800,'max_log_bytes':65536},'actor_sha256':digest(actor),'companion_sha256':digest(checker),'commands':commands,'executions':{k:{'exit':0,'process_group_retired':True} for k in commands},'environment':env,'owner_environment':{**env,'NUDOX_BUILD_STYLO_WORKER_ID':anchor},'toolchain':{'cargo':artifact(cargo),'rustc':artifact(rustc)},'apple_tools':{name:[1,'f'*64] for name in ('codesign','otool','lipo','xcrun','xcode-select','git')},'developer_dir':'/developer','sdk':{'selected':'/sdk','resolved':'/sdk','version':'15.5','settings':[1,'f'*64]},'selected_tools':{name:{'path':'/tool/'+name,'identity':[1,'f'*64]} for name in ('clang','otool','lipo','git')},'build_helpers':{name:{'selected':'/bin/'+name,'path':'/bin/'+name,'bytes':1,'sha256':'f'*64} for name in ('cc','make','python3','sh')},'worker':worker_record,'raw_worker':artifact(raw),'observer':artifact(observer),'owner':artifact(owner),'worker_anchor':anchor,'sign_command':['/usr/bin/codesign','--force','--timestamp','--options','runtime','--sign','Developer ID',str(worker)]}
            path=output/'receipt.json'
            def write():
                path.write_text(json.dumps(receipt))
                return digest(path)
            original_digest=companion.digest_regular
            def file_digest(p,maximum=companion.MAX_WORKER_BYTES,**kwargs):
                if str(p).startswith(('/usr/bin/','/sdk/','/tool/','/bin/')): return (1,'f'*64)
                return original_digest(p,maximum,**kwargs)
            def run(command,**kwargs):
                if command[0] == str(observer): return json.dumps({'state':'observed-helper','identity':anchor}).encode()
                if command[1:] == ['-p']: return b'/developer\n'
                if '--show-sdk-path' in command: return b'/sdk\n'
                if '--show-sdk-version' in command: return b'15.5\n'
                if '--find' in command: return ('/tool/'+command[-1]+'\n').encode()
                raise AssertionError(command)
            real_resolve=Path.resolve
            def resolve(p,*args,**kwargs):
                if str(p).startswith(('/sdk','/tool/','/bin/')): return p
                return real_resolve(p,*args,**kwargs)
            with patch.object(companion,'digest_regular',side_effect=file_digest), patch.object(companion,'run_tool',side_effect=run), patch.object(companion,'inspect_signed_worker',return_value={k:worker_record[k] for k in ('bytes','sha256','architecture','team_id','system_loads','signature_requirement')}), patch.object(companion,'inspect_system_executable',return_value={'bytes':observer.stat().st_size,'sha256':digest(observer)}), patch.object(companion.shutil,'which',side_effect=lambda name,path: '/bin/'+name), patch.object(Path,'resolve',resolve):
                accepted,_=companion.admit_bound_owner(path,source,src,'aarch64-apple-darwin',write(),digest(actor),team)
                self.assertEqual(accepted['worker_anchor'],anchor)
                for mutation in ('flags','anchor','source','sign','team'):
                    saved=json.loads(json.dumps(receipt))
                    if mutation == 'flags': receipt['environment']['CFLAGS']=''
                    if mutation == 'anchor': receipt['owner_environment']['NUDOX_BUILD_STYLO_WORKER_ID']='b'*64
                    if mutation == 'source': receipt['source']['tree']='9'*40
                    if mutation == 'sign': receipt['sign_command'][6]='-'
                    if mutation == 'team': receipt['worker']['team_id']='OTHERTEAM1'
                    with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                        companion.admit_bound_owner(path,source,src,'aarch64-apple-darwin',write(),digest(actor),team)
                    receipt.clear();receipt.update(saved)

class SigningPreservationTests(unittest.TestCase):
    def test_invalid_companion_preflight_does_not_probe_credentials(self):
        import macos_release
        with patch.object(macos_release.subprocess, 'check_output') as check, patch.object(macos_release.subprocess, 'run') as run:
            result = macos_release.preflight({'stylo_bound_receipt':'/missing/receipt'})
            self.assertFalse(result['ready'])
            self.assertIn('required together', result['blockers'][0])
            empty = macos_release.preflight({'expected_stylo_team_id':''})
            self.assertFalse(empty['ready'])
            self.assertIn('required together', empty['blockers'][0])
            malformed = macos_release.preflight({'stylo_bound_receipt':'/missing', 'expected_stylo_receipt_sha256':'bad', 'expected_stylo_actor_sha256':'a'*64, 'expected_stylo_team_id':'A1B2C3D4E5'})
            self.assertIn('malformed', malformed['blockers'][0])
            incompatible = macos_release.preflight({'stylo_bound_receipt':'/missing', 'expected_stylo_receipt_sha256':'a'*64, 'expected_stylo_actor_sha256':'b'*64, 'expected_stylo_team_id':'A1B2C3D4E5', 'relocation_plan':'/plan'})
            self.assertIn('non-relocating', incompatible['blockers'][0])
            check.assert_not_called()
            run.assert_not_called()

    def test_nested_signer_preserves_anchored_worker_and_signs_other_images(self):
        import macos_release
        worker = Path('/private/app/Contents/MacOS/backend-stylo-worker')
        owner = Path('/private/app/Contents/MacOS/backend-locald')
        with patch.object(macos_release.subprocess,'check_output',return_value='EXECUTE') as inspect, patch.object(macos_release,'run') as sign:
            macos_release.sign_nested_image(worker, worker, 'identity', Path('/private/entitlements'))
            inspect.assert_not_called()
            sign.assert_not_called()
            macos_release.sign_nested_image(owner, worker, 'identity', Path('/private/entitlements'))
            self.assertEqual(sign.call_count,1)
            self.assertEqual(sign.call_args.args[0][-1],str(owner))
