"""Fixture admission checks only; no actual Mac release or install QA."""
import copy
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import macos_managed_native_host as native
import macos_release as release


def load(name,filename):
    spec=importlib.util.spec_from_file_location(name,Path(__file__).with_name(filename))
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module

builder=load('native_test_builder','build-macos-investor-app.py')
bundle=load('native_test_bundle','macos-investor-bundle.py')


class ManagedNativeTests(unittest.TestCase):
    def fixture(self,root):
        source={'git_revision':'a'*40,'git_tree':'b'*40,'cargo_lock_sha256':'c'*64,'working_tree':'clean'}
        workspace=str(root/'source');evidence=root/'evidence';evidence.mkdir();artifacts=root/'artifacts';artifacts.mkdir()
        assets={key:'d'*64 for key in ['runner','runner.interpreter','runner.environment.0','runner.exec',*['runner.tool.'+n for n in ['CARGO','RUSTC','RUSTDOC','RUSTC_WRAPPER']]]}
        runner={'execution_kind':native.KIND,'path':'/fixture/runner','sha256':'d'*64,'expected_sha256':'d'*64,'invocation_prefix':['$CARGO_RUNNER_INTERPRETER','$CARGO_RUNNER'],'referenced_asset_sha256':assets}
        raw_wrapper={'schema':2,'workspace_root':workspace,'cargo_target_dir':workspace+'/.local/target','git_head':'a'*40,'cargo_lock_sha256':'c'*64,'cargo_lock_sha256_after':'c'*64,'source_changed_during_build':False,'source_dirty_sha256':'e'*64,'source_dirty_sha256_after':'e'*64,'cargo_exit_status':0,'toolchain':{'capture_complete':True,'changed_during_build':False,**{n:n+' 1.97.1' for n in ['cargo','rustc','rustdoc']}},'wrapper':{n+'_sha256':'d'*64 for n in ['runtime','source','rustc']},'features':{'features':[],'all_features':False,'no_default_features':False,'targets':[]},'run_id':'new-fixture-only','outputs':[]}
        inputs={'source':source,'files':[{'path':'Cargo.lock','mode':'100644','git_blob':'f'*40,'bytes':1,'sha256':'f'*64},{'path':'.config/scripts/cargo-shared-cache.sh','mode':'100755','git_blob':'f'*40,'bytes':1,'sha256':'d'*64}],'gitlinks':[],'total_blob_bytes':2}
        tools={n:{'sha256':'d'*64,'version':n+' 1.97.1','path':'/fixture/'+n} for n in ['cargo','rustc','rustdoc']};tools.update(host='aarch64-apple-darwin',rustc_vv='rustc 1.97.1\nhost: aarch64-apple-darwin\n')
        refs={}
        pin=lambda path:{'path':path,'sha256':'d'*64}
        plan={'schema':'nudox.macos-managed-native-host-plan.v1','environment':pin('/fixture/environment.sh'),
              'interpreter':pin('/bin/sh'),'tools':{name:pin('/fixture/'+name) for name in ('cargo','rustc','rustdoc')},
              'rustc_wrapper':pin('/fixture/wrapper'),'build_role':workspace+'/.local/build/role',
              'jobs':2,'lifetime_locks':['/fixture/owner.lock']}
        refs['plan']=native._reference(evidence,'plan.json',plan)
        for key,value in [('source_before',inputs),('source_after',inputs),('tools_before',tools),('tools_after',tools),('wrapper',raw_wrapper),('environment',{'CARGO_TARGET_DIR':workspace+'/.local/target','CARGO_BUILD_BUILD_DIR':workspace+'/.local/build/role','NUDOX_CARGO_BUILD_SLOTS':'4','RUSTC':'/fixture/rustc','RUSTDOC':'/fixture/rustdoc'})]:refs[key]=native._reference(evidence,key+'.json',value)
        metadata={'packages':[{'name':name,'manifest_path':workspace+'/'+native.PACKAGE_ROOTS[name]+'/Cargo.toml','targets':[{'name':name,'kind':['bin'],'src_path':workspace+'/'+native.PACKAGE_ROOTS[name]+'/src/main.rs'}]} for name in native.PACKAGES]}
        refs['metadata']=native._reference(evidence,'metadata.json',metadata)
        events={};outputs=[];executables={}
        for name in native.PACKAGES:
            image=artifacts/name;image.write_bytes(b'compiled '+name.encode());image.chmod(0o755);sha=hashlib.sha256(image.read_bytes()).hexdigest();executables[name]=sha
            events[name]={'reason':'compiler-artifact','manifest_path':workspace+'/'+native.PACKAGE_ROOTS[name]+'/Cargo.toml','target':{'name':name,'kind':['bin'],'src_path':workspace+'/'+native.PACKAGE_ROOTS[name]+'/src/main.rs'},'profile':{'opt_level':'3','debuginfo':0,'debug_assertions':False,'overflow_checks':False,'test':False},'fresh':False,'executable':workspace+'/.local/target/release/'+name}
            outputs.append({'path':native.output_name(name),'cargo_executable':events[name]['executable'],'sha256':sha,'size_bytes':image.stat().st_size})
        wrapper=builder.cargo_provenance(evidence,{p.name for p in evidence.glob('*.json')}-{'wrapper.json'},Path(workspace),source,None,Path(workspace)/'.local/target')
        proof={'schema':4,'kind':native.KIND,'target_mode':'native-host','target':'aarch64-apple-darwin','workspace_root':workspace,'build_role':workspace+'/.local/build/role','command':[*runner['invocation_prefix'],*native.build_arguments()],'retirement':'owned-child-kernel-wait','child_pid':123,'exit_status':0,'raw_evidence':refs,'managed_wrapper_provenance':wrapper,'cargo_artifacts':events,'outputs':outputs,'operator_plan_sha256':refs['plan']['sha256'],'record_sha256':'f'*64}
        data=(source,runner,proof,root/'application-build-receipt.json',artifacts)
        self.write_log(data,list(events.values()))
        receipt={'schema':1,'source':source,'source_before':source,'source_after':source,'source_unchanged':True,'target':'aarch64-apple-darwin','profile':'release','locked_build':True,'cargo_runner_before':runner,'cargo_runner_after':runner,'cargo_runner_unchanged':True,'cargo_provenance':proof,'command':proof['command'],'executables':executables}
        data[3].write_bytes(native.canonical(receipt));return data

    def write_log(self,data,events):
        raw=b''.join(native.canonical(e) for e in events);path=data[3].parent/'evidence/cargo.log';path.write_bytes(raw)
        data[2]['raw_evidence']['cargo_log']={'path':'evidence/cargo.log','sha256':native.digest(raw),'size_bytes':len(raw)}
        execution={'command':['/bin/sh','/fixture/runner',*native.build_arguments()],'cwd':data[2]['workspace_root'],
                   'child_pid':123,'returncode':0,'elapsed_ns':1,'started_at_utc':'fixture-start','finished_at_utc':'fixture-end',
                   'retirement':'owned-child-kernel-wait','output_log':{'path':'cargo.log','sha256':native.digest(raw),'size_bytes':len(raw)}}
        data[2]['raw_evidence']['execution']=native._reference(data[3].parent/'evidence','execution.json',execution)
        data[2]['record_sha256']=native.proof_digest(data[2])

    def validate(self,data):
        data[2]['record_sha256']=native.proof_digest(data[2])
        native.validate(data[2],data[0],'aarch64-apple-darwin',data[1],data[3])

    def test_accepts_release_four_through_existing_common_app_validator(self):
        with tempfile.TemporaryDirectory() as d:
            source,_,_,receipt,artifacts=self.fixture(Path(d))
            _,images=bundle.validate_app_build(receipt,artifacts,source,'aarch64-apple-darwin','d'*64)
            self.assertEqual(set(images),set(native.PACKAGES))

    def test_rejects_host_kind_schema_target_mode_and_recipe_relabel(self):
        for key,value in [('target','x86_64-apple-darwin'),('kind','direct-cargo'),('schema',3),('target_mode','explicit-target'),('command',['build','--target','aarch64-apple-darwin'])]:
            with self.subTest(key=key),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d));data[2][key]=value
                with self.assertRaises(ValueError):self.validate(data)

    def test_rejects_missing_raw_proof_and_unwaited_or_unsuccessful_child(self):
        for change in ['raw','wait','exit']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d))
                if change=='raw':(Path(d)/'evidence/cargo.log').unlink()
                elif change=='wait':data[2]['retirement']='assumed'
                else:data[2]['exit_status']=True
                with self.assertRaises((ValueError,RuntimeError,OSError)):self.validate(data)

    def test_rejects_changed_source_tool_or_effective_environment(self):
        for name in ['source_after','tools_after','environment','metadata']:
            with self.subTest(name=name),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d));value=json.loads((Path(d)/data[2]['raw_evidence'][name]['path']).read_text())
                if name=='environment':value['NUDOX_CARGO_BUILD_SLOTS']='6'
                elif name=='metadata':value['packages'][0]['targets'][0]['name']='wrong'
                else:value['changed']=True
                data[2]['raw_evidence'][name]=native._reference(Path(d)/'evidence',name+'.json',value)
                with self.assertRaises(ValueError):self.validate(data)

    def test_rejects_debug_test_missing_duplicate_wrong_source_or_oversize_artifacts(self):
        for change in ['debug','test','missing','duplicate','source','size']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d));proof=data[2];event=proof['cargo_artifacts'][native.PACKAGES[0]]
                if change=='debug':event['profile']['opt_level']='0'
                if change=='test':event['profile']['test']=True
                if change=='source':event['target']['src_path']='/other/main.rs'
                if change=='size':proof['outputs'][0]['size_bytes']=native.MAX_IMAGE+1
                events=list(proof['cargo_artifacts'].values())
                if change=='missing':events.pop()
                if change=='duplicate':events.append(events[0])
                self.write_log(data,events)
                with self.assertRaises(ValueError):self.validate(data)

    def test_managed_raw_stream_has_a_finite_bound_and_waits_owned_child(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);log=root/'cargo.log'
            with self.assertRaisesRegex(builder.BuildError,'finite log bound'):
                builder._stream_direct_cargo(['/bin/sh','-c',"printf 'long line'; sleep 1"],root,dict(os.environ),log,maximum_log_bytes=3)
            self.assertLessEqual(log.stat().st_size,3)

    def test_inspection_capture_refuses_before_retaining_oversize_output(self):
        with self.assertRaisesRegex(ValueError,'inspection output exceeds'):
            native.capture(['/bin/sh','-c',"printf 'too long'; sleep 1"],3)

    def test_rejects_wrapper_script_detached_from_actual_source_inventory(self):
        with tempfile.TemporaryDirectory() as d:
            data=self.fixture(Path(d));ref=data[2]['raw_evidence']['source_before'];value=json.loads((Path(d)/ref['path']).read_text());value['files'][1]['sha256']='0'*64
            for name in ('source_before','source_after'):data[2]['raw_evidence'][name]=native._reference(Path(d)/'evidence',name+'.json',value)
            with self.assertRaisesRegex(ValueError,'pins differ'):self.validate(data)

    def test_rejects_symlinked_evidence_root_before_open(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);data=self.fixture(root);(root/'evidence').rename(root/'other');(root/'evidence').symlink_to(root/'other',target_is_directory=True)
            with self.assertRaises(ValueError):self.validate(data)

    def test_rejects_changed_record_digest_or_raw_execution(self):
        for change in ['digest','cwd','command','returncode','pid','log']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d))
                if change=='digest':
                    data[2]['record_sha256']='0'*64
                    with self.assertRaisesRegex(ValueError,'evidence digest'):native.validate(data[2],data[0],'aarch64-apple-darwin',data[1],data[3])
                    continue
                execution=json.loads((Path(d)/data[2]['raw_evidence']['execution']['path']).read_text())
                if change=='cwd':execution['cwd']='/other'
                if change=='command':execution['command'].append('--target')
                if change=='returncode':execution['returncode']=True
                if change=='pid':execution['child_pid']+=1
                if change=='log':execution['output_log']['sha256']='0'*64
                data[2]['raw_evidence']['execution']=native._reference(Path(d)/'evidence','execution.json',execution)
                with self.assertRaisesRegex(ValueError,'retired invocation'):self.validate(data)

    def test_rejects_inconsistent_or_duplicate_source_inventory(self):
        for change in ['total','duplicate','gitlink','mode']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d));value=json.loads((Path(d)/data[2]['raw_evidence']['source_before']['path']).read_text())
                if change=='total':value['total_blob_bytes']+=1
                if change=='duplicate':value['files'].append(value['files'][0])
                if change=='gitlink':value['gitlinks'].append({'path':'vendor','mode':'160000','git_object':'0'*40,'state':'initialized'})
                if change=='mode':value['files'][0]['mode']='other'
                for name in ('source_before','source_after'):data[2]['raw_evidence'][name]=native._reference(Path(d)/'evidence',name+'.json',value)
                with self.assertRaises(ValueError):self.validate(data)

    def test_rejects_retained_plan_with_different_pins_or_jobs(self):
        for change in ['pin','jobs','tool-path','extra']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                data=self.fixture(Path(d));plan=json.loads((Path(d)/'evidence/plan.json').read_text())
                if change=='pin':plan['tools']['rustc']['sha256']='0'*64
                if change=='jobs':plan['jobs']=4
                if change=='tool-path':plan['tools']['rustc']['path']='/other/rustc'
                if change=='extra':plan['unknown']=True
                ref=native._reference(Path(d)/'evidence','plan.json',plan);data[2]['raw_evidence']['plan']=ref
                data[2]['operator_plan_sha256']=ref['sha256']
                with self.assertRaises(ValueError):self.validate(data)

    def tracked_fixture(self,root):
        subprocess.run(['git','init','-q',str(root)],check=True);(root/'Cargo.lock').write_bytes(b'lock');(root/'link').symlink_to('Cargo.lock');subprocess.run(['git','-C',str(root),'add','.'],check=True);subprocess.run(['git','-C',str(root),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','fixture'],check=True)

    def test_actual_source_blob_and_symlink_bytes_match_git_then_mutation_refuses(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);self.tracked_fixture(root);self.assertEqual(len(native.tracked_inputs(root,{})['files']),2);(root/'Cargo.lock').write_bytes(b'changed')
            with self.assertRaisesRegex(ValueError,'Git blob'):native.tracked_inputs(root,{})

    def test_actual_source_walk_stops_at_finite_entry_and_byte_bounds(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);self.tracked_fixture(root)
            with patch.object(native,'MAX_INPUTS',1),self.assertRaisesRegex(ValueError,'entry bound'):native.tracked_inputs(root,{})
            with patch.object(native,'MAX_SOURCE',1),self.assertRaises(ValueError):native.tracked_inputs(root,{})

    def test_uninitialized_gitlink_records_object_without_following_a_dangling_link(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);self.tracked_fixture(root)
            commit=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip()
            subprocess.run(['git','-C',str(root),'update-index','--add','--cacheinfo','160000,'+commit+',vendor'],check=True)
            subprocess.run(['git','-C',str(root),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','gitlink fixture'],check=True)
            self.assertEqual(native.tracked_inputs(root,{})['gitlinks'],[{'path':'vendor','mode':'160000','git_object':commit,'state':'uninitialized'}])
            (root/'vendor').symlink_to('missing-directory')
            with self.assertRaisesRegex(ValueError,'gitlinks require'):native.tracked_inputs(root,{})

    def producer_fixture(self, root, change=None):
        """Fake tool outputs exercise production orchestration, never a compiler."""
        root=root.resolve()
        source=root/'source';source.mkdir();self.tracked_fixture(source)
        (source/'.gitignore').write_text('.local/\n')
        cache=source/'.config/scripts/cargo-shared-cache.sh';cache.parent.mkdir(parents=True)
        cache.write_text('# fixture cache wrapper\n');cache.chmod(0o755)
        for package in native.PACKAGES:
            path=source/native.PACKAGE_ROOTS[package];(path/'src').mkdir(parents=True)
            (path/'Cargo.toml').write_text('# fixture manifest\n');(path/'src/main.rs').write_text('// fixture source\n')
        subprocess.run(['git','-C',str(source),'add','.'],check=True)
        subprocess.run(['git','-C',str(source),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','-qm','producer fixture'],check=True)
        revision=subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD'],text=True).strip()
        tree=subprocess.check_output(['git','-C',str(source),'rev-parse','HEAD^{tree}'],text=True).strip()
        metadata={'packages':[{'name':name,'manifest_path':str(source/native.PACKAGE_ROOTS[name]/'Cargo.toml'),
            'targets':[{'name':name,'kind':['bin'],'src_path':str(source/native.PACKAGE_ROOTS[name]/'src/main.rs')}]} for name in native.PACKAGES]}
        tool_dir=root/'tools';tool_dir.mkdir();tools={}
        for name in ('cargo','rustc','rustdoc'):
            path=tool_dir/name;special=json.dumps(metadata) if name=='cargo' else 'rustc 1.97.1\nhost: aarch64-apple-darwin'
            path.write_text("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n  printf '%s\\n' '"+name+" 1.97.1'\nelse\n  cat <<'FIXTURE_OUTPUT'\n"+special+"\nFIXTURE_OUTPUT\nfi\n")
            path.chmod(0o755);tools[name]=path
        environment=root/'environment.sh';environment.write_text('export PATH=/usr/bin:/bin\n')
        wrapper=root/'rustc-wrapper';wrapper.write_text('# fixture wrapper\n')
        runner=root/'cargo-runner';runner.write_text('#!/bin/sh\n# fixture runner, not executed\n')
        locks=[root/'owner.lock',root/'source.lock']
        for lock in locks:lock.touch(mode=0o600)
        pin=lambda path:{'path':str(path),'sha256':builder.sha256(path)}
        plan={'schema':'nudox.macos-managed-native-host-plan.v1','environment':pin(environment),
              'interpreter':pin(Path('/bin/sh')),'tools':{name:pin(path) for name,path in tools.items()},
              'rustc_wrapper':pin(wrapper),'build_role':str(source/'.local/build/role'),'jobs':2,
              'lifetime_locks':[str(path) for path in locks]}
        plan_path=root/'plan.json';plan_path.write_bytes(native.canonical(plan))
        args=SimpleNamespace(source_root=source,output_dir=root/'build',managed_native_host_plan=plan_path,
            expected_managed_plan_sha256=builder.sha256(plan_path),cargo_runner=runner,
            expected_runner_sha256=builder.sha256(runner),target='aarch64-apple-darwin',
            expected_revision=revision,expected_tree=tree)
        observed=[]
        def fake_stream(command,cwd,env,log_path,maximum_log_bytes):
            observed.append((command,cwd,env,maximum_log_bytes))
            events=[]
            for name in native.PACKAGES:
                path=source/'.local/target/release'/name;path.parent.mkdir(parents=True,exist_ok=True)
                path.write_bytes(b'fixture optimized '+name.encode());path.chmod(0o755)
                events.append({'reason':'compiler-artifact','manifest_path':str(source/native.PACKAGE_ROOTS[name]/'Cargo.toml'),
                    'target':{'name':name,'kind':['bin'],'src_path':str(source/native.PACKAGE_ROOTS[name]/'src/main.rs')},
                    'profile':{'opt_level':'3','debuginfo':0,'debug_assertions':False,'overflow_checks':False,'test':False},
                    'fresh':name=='backend-cli','executable':str(path)})
            if change=='missing':events.pop()
            raw=b''.join(native.canonical(event) for event in events);log_path.write_bytes(raw)
            provenance=source/'.local/target/.nudox-provenance';provenance.mkdir()
            record={'schema':2,'workspace_root':str(source),'cargo_target_dir':str(source/'.local/target'),
                'git_head':revision,'cargo_lock_sha256':builder.sha256(source/'Cargo.lock'),
                'cargo_lock_sha256_after':builder.sha256(source/'Cargo.lock'),'source_changed_during_build':False,
                'source_dirty_sha256':'e'*64,'source_dirty_sha256_after':'e'*64,'cargo_exit_status':0,
                'toolchain':{'capture_complete':True,'changed_during_build':False,**{name:name+' 1.97.1' for name in tools}},
                'wrapper':{'runtime_sha256':builder.sha256(runner),'source_sha256':builder.sha256(cache),'rustc_sha256':builder.sha256(wrapper)},
                'features':{'features':[],'all_features':False,'no_default_features':False,'targets':[]},'run_id':'producer-fixture','outputs':[]}
            (provenance/'new.json').write_bytes(native.canonical(record))
            if change=='source':(source/'Cargo.lock').write_bytes(b'mutated')
            if change=='tool':tools['rustc'].write_text(tools['rustc'].read_text()+'# changed\n')
            if change=='lock':locks[0].rename(root/'old.lock');locks[0].touch(mode=0o600)
            return {'returncode':101 if change=='exit' else 0,'child_pid':123,'started_at_utc':'fixture-start',
                'finished_at_utc':'fixture-end','elapsed_ns':1,
                'output_log':{'path':'cargo.log','sha256':native.digest(raw),'size_bytes':len(raw)}}
        return args,locks,observed,fake_stream

    def test_producer_retains_new_raw_recipe_and_common_validator_accepts_it(self):
        with tempfile.TemporaryDirectory() as d:
            args,locks,observed,fake_stream=self.producer_fixture(Path(d));api=dict(vars(builder));api['_stream_direct_cargo']=fake_stream
            self.assertEqual(native.build(args,api),0)
            command,cwd,env,maximum=observed[0]
            self.assertEqual(command,['/bin/sh',str(args.cargo_runner),*native.build_arguments()])
            self.assertNotIn('--target',command);self.assertEqual(cwd,args.source_root)
            self.assertEqual(env['NUDOX_CARGO_BUILD_SLOTS'],'4');self.assertEqual(maximum,native.MAX_LOG)
            receipt_path=args.output_dir/'application-build-receipt.json';receipt=json.loads(receipt_path.read_text())
            _,images=bundle.validate_app_build(receipt_path,args.output_dir/'artifacts',receipt['source'],args.target,args.expected_runner_sha256)
            self.assertEqual(set(images),set(native.PACKAGES))
            self.assertTrue(receipt['cargo_provenance']['cargo_artifacts']['backend-cli']['fresh'])
            for lock in locks:
                with lock.open('r+') as handle:fcntl.flock(handle,fcntl.LOCK_EX|fcntl.LOCK_NB)

    def test_producer_failure_keeps_raw_evidence_without_success_receipt_and_releases_locks(self):
        for change in ['exit','source','tool','missing','lock']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                args,locks,observed,fake_stream=self.producer_fixture(Path(d),change);api=dict(vars(builder));api['_stream_direct_cargo']=fake_stream
                with self.assertRaises((ValueError,builder.BuildError)):native.build(args,api)
                self.assertEqual(len(observed),1);self.assertTrue((args.output_dir/'evidence/cargo.log').exists())
                self.assertFalse((args.output_dir/'application-build-receipt.json').exists())
                self.assertFalse(json.loads((args.output_dir/'evidence/failure.json').read_text())['application_build_receipt_emitted'])
                for lock in locks:
                    with lock.open('r+') as handle:fcntl.flock(handle,fcntl.LOCK_EX|fcntl.LOCK_NB)

    def test_release_preflight_rejects_unpinned_or_linked_managed_plan(self):
        for change in ['pin','symlink','oversize','missing-plan']:
            with self.subTest(change=change),tempfile.TemporaryDirectory() as d:
                root=Path(d);source=root/'source';source.mkdir();plan=root/'plan.json';plan.write_bytes(b'{}')
                config={'source_root':str(source),'output_dir':str(root/'release'),
                        'managed_native_host_plan':str(plan),'expected_managed_plan_sha256':'0'*64}
                if change=='symlink':plan.rename(root/'other.json');plan.symlink_to(root/'other.json')
                if change=='oversize':plan.write_bytes(b' ' * 65537)
                if change=='missing-plan':del config['managed_native_host_plan']
                with patch.object(release.shutil,'which',return_value=None),patch.object(release.subprocess,'check_output',return_value=''):
                    report=release.preflight(config)
                self.assertFalse(report['ready'])
                self.assertTrue(any('managed-native-host plan' in message for message in report['blockers']))

    def test_release_driver_forwards_plan_only_to_new_build_recipe(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);source=root/'source';source.mkdir()
            config={'source_root':str(source),'output_dir':str(root/'release'),'sdk_only':True,
                    'expected_revision':'a'*40,'expected_tree':'b'*40,'expected_runner_sha256':'c'*64,
                    'expected_icon_sha256':'d'*64,'minimum_os':'26.0',
                    'managed_native_host_plan':str(root/'plan.json'),'expected_managed_plan_sha256':'e'*64}
            config.update({key:str(root/key) for key in release.release_inputs(config)})
            with patch.object(release,'preflight',return_value={'ready':True}),patch.object(release,'run') as run,patch.object(release,'finalize'):
                release.prepare(config)
            build=run.call_args_list[0].args[0];package=run.call_args_list[1].args[0]
            self.assertEqual(build[-4:],['--managed-native-host-plan',config['managed_native_host_plan'],
                                       '--expected-managed-plan-sha256','e'*64])
            self.assertNotIn('--managed-native-host-plan',package)

if __name__=='__main__':unittest.main()
