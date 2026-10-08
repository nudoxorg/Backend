"""Closed proof for a new native-host build through an already pinned runner.

This is not an adapter for historical receipts.  The existing explicit-target
producer and its schema-2/3 admission remain separate.
"""
from __future__ import annotations

import hashlib
import importlib.util
import fcntl
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import signal
import stat
import subprocess
import tomllib

from linux_release_package import admit_file_digest, read_regular_bytes

KIND = 'managed-native-host'
SCHEMA = 4
MAX_IMAGE = 512 * 1024**2
MAX_LOG = 256 * 1024**2
MAX_SOURCE = 2 * 1024**3
MAX_INPUTS = 65536
PACKAGES = ('backend-desktop', 'backend-cli', 'backend-mcp', 'backend-locald')
PACKAGE_ROOTS = {'backend-desktop': 'apps/desktop', 'backend-cli': 'apps/cli',
                 'backend-mcp': 'apps/mcp', 'backend-locald': 'apps/locald'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(',', ':')) + '\n').encode()


def capture(command, maximum, **kwargs):
    """Retain only bounded inspection output and wait the exact owned child."""
    process=subprocess.Popen(command,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,
                             start_new_session=True,**kwargs)
    try:
        raw=process.stdout.read(maximum+1)
        require(len(raw)<=maximum,'native-host inspection output exceeds its bound')
        require(process.wait()==0,'native-host inspection command failed: '+str(command[0]))
        return raw
    finally:
        if process.poll() is None:
            # The unreaped owned leader retains this process-group identity.
            try:os.killpg(process.pid,signal.SIGKILL)
            except ProcessLookupError:pass
            process.wait()
        process.stdout.close()


def source_git(source, *arguments):
    """Inspect this checkout, without operator Git config or worktree redirects."""
    environment={key:value for key,value in os.environ.items() if not key.startswith('GIT_')}
    environment.update(GIT_CONFIG_GLOBAL='/dev/null',GIT_CONFIG_SYSTEM='/dev/null',GIT_CONFIG_NOSYSTEM='1')
    command=['git','-C',str(source),'--work-tree='+str(source),'-c','core.worktree='+str(source),
             '-c','core.excludesFile=/dev/null','-c','core.fsmonitor=false',*arguments]
    return capture(command,16*1024**2,env=environment)


def source_identity(source, revision, tree):
    head=source_git(source,'rev-parse','HEAD').decode().strip()
    actual_tree=source_git(source,'rev-parse','HEAD^{tree}').decode().strip()
    status=source_git(source,'status','--porcelain','--untracked-files=all').strip()
    require(head==revision and actual_tree==tree and not status,
            'source must remain at the exact operator-pinned clean commit')
    # ls-files --others deliberately has no exclude options: .gitignore and
    # .git/info/exclude must not hide an auto-discovered build.rs or source file.
    roots=[];manifest_bytes=0
    entries=source_git(source,'ls-tree','-rz','HEAD').split(b'\0')
    require(len(entries)-1<=MAX_INPUTS,'tracked source exceeds its entry bound')
    for entry in entries:
        if not entry:continue
        header,name=entry.split(b'\t',1)
        if not name.endswith(b'Cargo.toml') or header.split()[1]!=b'blob':continue
        path=source/os.fsdecode(name)
        raw=read_regular_bytes(path,min(1024**2,MAX_SOURCE-manifest_bytes),'tracked Cargo manifest')
        manifest_bytes+=len(raw)
        try:package='package' in tomllib.loads(raw.decode())
        except tomllib.TOMLDecodeError:
            # Cargo may accept TOML syntax newer than Python's parser. This
            # inspection only selects directories for the no-exclude input
            # check; conservatively include an undecoded manifest directory.
            # The selected Cargo still validates the full manifest later.
            package=True
        if package:roots.append(':(literal)'+str(path.parent.relative_to(source)))
    if roots:
        require(not source_git(source,'ls-files','--others','-z','--',*roots),
                'source contains untracked or ignored Cargo package inputs')
    return {'git_revision':head,'git_tree':actual_tree,
            'cargo_lock_sha256':admit_file_digest(source/'Cargo.lock',MAX_IMAGE)[1],
            'working_tree':'clean'}


def owned_graph_directory(source, path, must_exist=False):
    """Reject redirected existing components; a missing owned tail may be created."""
    require(path.is_absolute() and '..' not in path.parts and path.is_relative_to(source/'.local'),
            'native-host graph directory is not workspace owned')
    current=source
    for part in path.relative_to(source).parts:
        current=current/part
        try:metadata=current.lstat()
        except FileNotFoundError:continue
        require(stat.S_ISDIR(metadata.st_mode) and metadata.st_uid==os.geteuid(),
                'native-host graph directory has a symlink, non-directory or foreign ancestor')
    require(path.resolve(strict=False)==path,'native-host graph directory resolves outside the selected path')
    require(not must_exist or path.is_dir(),'native-host actual graph directory is missing')
    return str(path)


def build_arguments():
    result = ['build', '--release', '--locked', '--offline', '-j2', '--message-format=json']
    for package in PACKAGES:
        result.extend(['-p', package, '--bin', package])
    return result


def output_name(name):
    return 'native-host/release/' + name


def proof_digest(proof):
    return digest(canonical({'kind': KIND, 'raw_evidence': proof['raw_evidence']}))


def plan_assets(plan):
    return [('environment.0',plan['environment']),('interpreter',plan['interpreter']),
            ('tool.RUSTC_WRAPPER',plan['rustc_wrapper']),
            *[('tool.'+name.upper(),value) for name,value in plan['tools'].items()]]


def read_plan(raw):
    plan=json.loads(raw)
    require(isinstance(plan,dict) and set(plan)=={'schema','environment','interpreter','tools','rustc_wrapper','build_role','jobs','build_slots','cargo_home','cargo_configs','lifetime_locks'}
            and plan['schema']=='nudox.macos-managed-native-host-plan.v1'
            and type(plan['jobs']) is int and plan['jobs']==2
            and type(plan['build_slots']) is int and plan['build_slots'] in {4,6}
            and isinstance(plan['tools'],dict) and set(plan['tools'])=={'cargo','rustc','rustdoc'},
            'managed native-host plan is not closed or changes tools/jobs')
    require(isinstance(plan['cargo_home'],str) and Path(plan['cargo_home']).is_absolute()
            and '..' not in Path(plan['cargo_home']).parts and isinstance(plan['cargo_configs'],list),
            'managed native-host Cargo home/config inventory is missing')
    for label,entry in plan_assets(plan):
        require(isinstance(entry,dict) and set(entry)=={'path','sha256'}
                and isinstance(entry['path'],str) and Path(entry['path']).is_absolute()
                and isinstance(entry['sha256'],str) and re.fullmatch('[0-9a-f]{64}',entry['sha256']) is not None,
                'managed native-host asset pin is malformed: '+label)
    require(isinstance(plan['lifetime_locks'],list) and 1<=len(plan['lifetime_locks'])<=3
            and all(isinstance(path,str) and Path(path).is_absolute() for path in plan['lifetime_locks'])
            and len(set(plan['lifetime_locks']))==len(plan['lifetime_locks']),
            'managed native-host lifetime lock set differs')
    return plan


def runner_cargo(raw, selected):
    """Admit the two literal dispatch sites in the pinned managed runner.

    The exact reviewed script digest remains mandatory. This is a dispatch
    identity check, not an interpreter for arbitrary shell programs.
    """
    forwarding = [line.strip() for line in raw.decode('utf-8').splitlines()
                  if re.search(r'"\$@"(?:\s*&)?\s*$',line) and not line.lstrip().startswith('#')]
    require(forwarding == ['exec '+selected+' "$@"',selected+' "$@" &'],
            'managed runner Cargo dispatch differs from the selected Cargo')
    require(Path(selected).is_absolute() and re.fullmatch(r'/[A-Za-z0-9_./+-]+',selected) is not None,
            'managed runner Cargo path is not a literal absolute executable')
    return selected


def config_paths(workspace, cargo_home):
    roots=[Path(workspace),*Path(workspace).parents]
    require(len(roots)<=64,'native-host Cargo ancestor config search exceeds its bound')
    return list(dict.fromkeys(str(root/'.cargo'/name) for root in roots for name in ('config','config.toml'))) + [
        str(Path(cargo_home)/name) for name in ('config','config.toml')
        if str(Path(cargo_home)/name) not in {str(root/'.cargo'/part) for root in roots for part in ('config','config.toml')}]


def config_pins(inventory):
    return [{key:value for key,value in entry.items() if key!='content'} for entry in inventory]


def validate_configs(inventory, plan, workspace):
    require(isinstance(inventory,list) and [entry.get('path') for entry in inventory]==config_paths(workspace,plan['cargo_home'])
            and config_pins(inventory)==plan['cargo_configs'],
            'native-host effective Cargo config pins/absence witnesses differ')
    total=0
    def no_include(value):
        if isinstance(value,dict):return all(key!='include' and no_include(child) for key,child in value.items())
        if isinstance(value,list):return all(no_include(child) for child in value)
        return True
    for entry in inventory:
        if entry.get('state')=='absent':
            require(set(entry)=={'path','state'},'native-host absent Cargo config witness is malformed')
            continue
        require(set(entry)=={'path','state','sha256','size_bytes','content'} and entry['state']=='file'
                and isinstance(entry['content'],str),'native-host Cargo config entry is malformed')
        raw=entry['content'].encode();total+=len(raw)
        require(len(raw)<=65536 and total<=512*1024 and entry['size_bytes']==len(raw)
                and entry['sha256']==digest(raw),'native-host Cargo config exceeds its bound or changed')
        require(no_include(tomllib.loads(entry['content'])),
                'native-host recursive Cargo config include is not admitted')


def capture_configs(workspace, plan):
    inventory=[];total=0
    for name in config_paths(workspace,plan['cargo_home']):
        path=Path(name)
        for parent in path.parents:
            if parent.is_symlink():raise ValueError('native-host Cargo config ancestor is a symlink')
        try:metadata=path.lstat()
        except FileNotFoundError:inventory.append({'path':name,'state':'absent'});continue
        require(stat.S_ISREG(metadata.st_mode),'native-host Cargo config is not a regular file')
        raw=read_regular_bytes(path,min(65536,512*1024-total),'native-host Cargo config');total+=len(raw)
        inventory.append({'path':name,'state':'file','sha256':digest(raw),'size_bytes':len(raw),'content':raw.decode()})
    validate_configs(inventory,plan,workspace)
    return inventory


def validate_source_inventory(value, source):
    require(isinstance(value, dict) and set(value) == {'source', 'files', 'gitlinks', 'total_blob_bytes'}
            and value['source'] == source and isinstance(value['files'], list)
            and isinstance(value['gitlinks'], list)
            and 0 < len(value['files']) <= MAX_INPUTS
            and len(value['files']) + len(value['gitlinks']) <= MAX_INPUTS,
            'native-host actual tracked source inventory is missing or malformed')
    paths = set(); total = 0
    for entry in [*value['files'], *value['gitlinks']]:
        require(isinstance(entry, dict) and isinstance(entry.get('path'), str),
                'native-host tracked source entry is malformed')
        relative = PurePosixPath(entry['path'])
        require(entry['path'] and not relative.is_absolute() and '..' not in relative.parts
                and str(relative) == entry['path'] and entry['path'] not in paths,
                'native-host tracked source path is unsafe or repeated')
        paths.add(entry['path'])
        if entry.get('mode') == '160000':
            require(set(entry) == {'path','mode','git_object','state'} and entry['state'] == 'uninitialized'
                    and re.fullmatch('[0-9a-f]{40}', entry['git_object']) is not None,
                    'native-host Git link identity is malformed')
        else:
            require(set(entry) == {'path','mode','git_blob','bytes','sha256'}
                    and entry['mode'] in {'100644','100755','120000'}
                    and re.fullmatch('[0-9a-f]{40}', entry['git_blob']) is not None
                    and re.fullmatch('[0-9a-f]{64}', entry['sha256']) is not None
                    and type(entry['bytes']) is int and 0 <= entry['bytes'] <= MAX_IMAGE
                    and (entry['mode'] != '120000' or entry['bytes'] <= 4096),
                    'native-host tracked blob identity is malformed')
            total += entry['bytes']
    require(type(value['total_blob_bytes']) is int and 0 < total <= MAX_SOURCE
            and value['total_blob_bytes'] == total,
            'native-host tracked source total differs')


def tracked_inputs(source, identity):
    """Hash actual tracked bytes against their Git blob IDs, with finite I/O."""
    raw = source_git(source,'ls-tree','-rz','HEAD')
    files = []; links = []; total = 0
    entries = raw.split(b'\0')
    require(len(entries) - 1 <= MAX_INPUTS, 'tracked source exceeds its entry bound')
    for entry in entries:
        if not entry:
            continue
        header, name = entry.split(b'\t', 1)
        mode, kind, oid = header.decode().split()
        relative = PurePosixPath(os.fsdecode(name))
        require(not relative.is_absolute() and '..' not in relative.parts,
                'unsafe tracked source path')
        path = source.joinpath(*relative.parts)
        if mode == '160000' and kind == 'commit':
            require(not path.is_symlink() and (not path.exists() or (path.is_dir()
                    and not any(path.iterdir()))), 'initialized gitlinks require their own closed proof')
            links.append({'path':str(relative), 'mode':mode, 'git_object':oid,
                          'state':'uninitialized'})
            continue
        require(kind == 'blob' and mode in {'100644', '100755', '120000'},
                'unsupported tracked source kind')
        metadata = path.lstat()
        if mode == '120000':
            require(stat.S_ISLNK(metadata.st_mode), 'tracked symlink changed kind')
            content = os.fsencode(os.readlink(path))
            require(len(content) <= 4096, 'tracked symlink exceeds its bound')
        else:
            require(stat.S_ISREG(metadata.st_mode), 'tracked blob changed kind')
            content = read_regular_bytes(path, min(MAX_IMAGE, MAX_SOURCE-total),
                                         'tracked source blob')
        total += len(content)
        require(total <= MAX_SOURCE, 'tracked source exceeds its byte bound')
        blob = hashlib.sha1(b'blob ' + str(len(content)).encode() + b'\0' + content).hexdigest()
        require(blob == oid, 'tracked bytes differ from selected Git blob: ' + str(relative))
        files.append({'path':str(relative), 'mode':mode, 'git_blob':oid,
                      'bytes':len(content), 'sha256':digest(content)})
    result = {'source':identity, 'files':files, 'gitlinks':links, 'total_blob_bytes':total}
    validate_source_inventory(result, identity)
    return result


def _raw_reference(root, name, raw):
    path = root / name
    path.write_bytes(raw)
    return {'path':'evidence/' + name, 'sha256':digest(raw), 'size_bytes':len(raw)}


def _reference(root, name, value):
    return _raw_reference(root,name,canonical(value))


def _read_ref(receipt_path, ref, maximum):
    require(isinstance(ref, dict) and set(ref) == {'path','sha256','size_bytes'},
            'native-host evidence reference is not closed')
    relative = PurePosixPath(ref['path'])
    require(not relative.is_absolute() and '..' not in relative.parts
            and relative.parts[:1] == ('evidence',), 'unsafe native-host evidence reference')
    path = receipt_path.parent.joinpath(*relative.parts)
    for parent in [receipt_path.parent, *[receipt_path.parent.joinpath(*relative.parts[:n]) for n in range(1,len(relative.parts))]]:
        require(stat.S_ISDIR(parent.lstat().st_mode), 'native-host evidence directory is not a real directory')
    raw = read_regular_bytes(path, maximum, 'native-host evidence')
    require(len(raw) == ref['size_bytes'] and digest(raw) == ref['sha256'],
            'native-host raw evidence changed')
    return raw


def validate(proof, source, target, runner, receipt_path, expected_plan_sha256):
    """Validate only the new kind; caller retains common app/runner/image checks."""
    require(proof.get('schema') == SCHEMA and proof.get('kind') == KIND
            and proof.get('target_mode') == 'native-host' and proof.get('target') == target,
            'native-host provenance kind/host differs')
    require(proof.get('command') == [*runner['invocation_prefix'], *build_arguments()]
            and proof.get('retirement') == 'owned-child-kernel-wait'
            and type(proof.get('child_pid')) is int and proof['child_pid'] > 0
            and proof.get('exit_status') == 0 and type(proof['exit_status']) is int,
            'native-host proof lacks exact recipe or successful kernel wait')
    refs = proof.get('raw_evidence')
    require(isinstance(refs, dict) and set(refs) == {'source_before','source_after','tools_before','tools_after','cargo_log','wrapper','runner_script','environment_script','configs_before','configs_after','environment','metadata','execution','plan','graph_after'},
            'native-host proof lacks its raw evidence')
    require(proof.get('record_sha256') == proof_digest(proof),
            'native-host raw evidence digest differs')
    before = json.loads(_read_ref(receipt_path, refs['source_before'], 16*1024**2))
    after = json.loads(_read_ref(receipt_path, refs['source_after'], 16*1024**2))
    validate_source_inventory(before, source)
    require(before == after, 'native-host actual tracked source changed')
    workspace=proof.get('workspace_root');role=proof.get('build_role')
    require(isinstance(workspace,str) and Path(workspace).is_absolute() and '..' not in Path(workspace).parts
            and isinstance(role,str) and '..' not in Path(role).parts
            and Path(role).is_relative_to(Path(workspace)/'.local'),
            'native-host build role is not workspace owned')
    plan_raw=_read_ref(receipt_path,refs['plan'],65536);plan=read_plan(plan_raw)
    require(isinstance(expected_plan_sha256,str) and re.fullmatch('[0-9a-f]{64}',expected_plan_sha256) is not None
            and digest(plan_raw)==proof.get('operator_plan_sha256')==expected_plan_sha256 and plan['build_role']==role
            and all(runner['referenced_asset_sha256'].get('runner.'+label)==entry['sha256']
                    for label,entry in plan_assets(plan)),
            'native-host retained operator plan differs from selected pins')
    runner_raw=_read_ref(receipt_path,refs['runner_script'],128*1024)
    require(digest(runner_raw)==runner['sha256'] and runner_raw.splitlines()[0]==('#!'+plan['interpreter']['path']).encode(),
            'native-host retained runner differs from selected pin/interpreter')
    runner_cargo(runner_raw,plan['tools']['cargo']['path'])
    configs=json.loads(_read_ref(receipt_path,refs['configs_before'],2*1024**2))
    validate_configs(configs,plan,workspace)
    require(configs==json.loads(_read_ref(receipt_path,refs['configs_after'],2*1024**2)),
            'native-host effective Cargo config changed during the build')
    execution=json.loads(_read_ref(receipt_path, refs['execution'],1024**2))
    require(isinstance(execution,dict) and execution.get('cwd') == workspace
            and execution.get('child_pid') == proof['child_pid']
            and type(execution.get('child_pid')) is int
            and type(execution.get('returncode')) is int and execution['returncode'] == 0
            and type(execution.get('elapsed_ns')) is int and execution['elapsed_ns'] >= 0
            and execution.get('retirement') == 'owned-child-kernel-wait'
            and isinstance(execution.get('command'),list) and len(execution['command']) >= 2
            and execution['command'][:2] == [plan['interpreter']['path'],runner.get('path')]
            and isinstance(runner.get('path'),str) and Path(runner['path']).is_absolute()
            and execution['command'][2:] == build_arguments()
            and isinstance(execution.get('started_at_utc'),str) and execution['started_at_utc']
            and isinstance(execution.get('finished_at_utc'),str) and execution['finished_at_utc']
            and execution.get('output_log') == {**refs['cargo_log'], 'path':'cargo.log'},
            'native-host raw execution does not bind the exact retired invocation')
    tools = json.loads(_read_ref(receipt_path, refs['tools_before'], 1024**2))
    require(tools == json.loads(_read_ref(receipt_path, refs['tools_after'], 1024**2))
            and set(tools) == {'cargo','rustc','rustdoc','rustc_vv','host'} and tools['host'] == target
            and ('host: ' + target) in tools['rustc_vv'].splitlines(),
            'native-host actual compiler host/toolchain changed')
    assets = runner['referenced_asset_sha256']
    for name in ('cargo','rustc','rustdoc'):
        tool = tools[name]
        require(isinstance(tool, dict) and tool.get('sha256') == assets['runner.tool.'+name.upper()]
                and tool.get('path') == plan['tools'][name]['path']
                and isinstance(tool.get('version'),str) and tool['version'].strip(),
                'native-host actual tool bytes differ from pin')
    wrapper_raw = _read_ref(receipt_path, refs['wrapper'], 8*1024**2)
    raw_wrapper=json.loads(wrapper_raw);raw_toolchain=raw_wrapper['toolchain']
    actual_build=raw_wrapper.get('cargo_build_dir')
    require(actual_build in {str(Path(role)/'.nudox-cargo'/('slot-'+str(index))) for index in range(min(4,plan['build_slots']))},
            'native-host actual managed graph escaped the selected role/slot bounds')
    require(proof.get('physical_graph_directories')=={'target':workspace+'/.local/target','release':workspace+'/.local/target/release',
                                                     'provenance':workspace+'/.local/target/.nudox-provenance',
                                                     'build_role':role,'actual_build':actual_build},
            'native-host physical graph paths differ from the selected owned paths')
    require(json.loads(_read_ref(receipt_path,refs['graph_after'],65536))==proof['physical_graph_directories'],
            'native-host retained physical graph observation differs')
    for name in ('cargo','rustc','rustdoc'):
        initial=raw_toolchain.get('executables_before',{}).get(name,{})
        final=raw_toolchain.get('executables_after',{}).get(name,{})
        require(raw_toolchain.get(name+'_path')==tools[name]['path']
                and initial.get('path')==tools[name]['path'] and initial.get('sha256')==tools[name]['sha256']
                and initial.get('version')==tools[name]['version']
                and isinstance(initial.get('resolved_path'),str) and Path(initial['resolved_path']).is_absolute()
                and final=={key:initial[key] for key in ('path','resolved_path','sha256')},
                'native-host raw wrapper selected executable identity differs')
    # Reuse the existing wrapper validator, including source/lock/tool/feature
    # and output checks.  None denotes a genuine native-host empty target list.
    spec=importlib.util.spec_from_file_location('native_host_wrapper_validator',Path(__file__).with_name('build-macos-investor-app.py'))
    legacy=importlib.util.module_from_spec(spec);spec.loader.exec_module(legacy)
    wrapper_path=receipt_path.parent/refs['wrapper']['path'];directory=wrapper_path.parent
    previous={p.name for p in directory.glob('*.json')} - {wrapper_path.name}
    wrapper=legacy.cargo_provenance(directory,previous,Path(workspace),source,None,Path(workspace)/'.local/target')
    require(wrapper == proof.get('managed_wrapper_provenance'), 'native-host managed wrapper evidence differs')
    cache_inputs=[entry for entry in before['files'] if entry.get('path')=='.config/scripts/cargo-shared-cache.sh']
    require(len(cache_inputs)==1 and wrapper['wrapper_sha256']=={'runtime':runner['sha256'],'source':cache_inputs[0]['sha256'],'rustc':assets['runner.tool.RUSTC_WRAPPER']}
            and all(wrapper['toolchain'][name]==tools[name]['version'] for name in ('cargo','rustc','rustdoc')),
            'native-host managed wrapper/tool/source pins differ')
    environment=json.loads(_read_ref(receipt_path, refs['environment'],1024**2))
    static_raw=_read_ref(receipt_path,refs['environment_script'],128*1024)
    require(digest(static_raw)==plan['environment']['sha256'],
            'native-host retained static environment differs from plan')
    static=legacy._static_environment(receipt_path.parent/refs['environment_script']['path'])
    require(isinstance(environment,dict) and all(isinstance(environment.get(key),str) and Path(environment[key]).is_absolute() for key in ('HOME','TMPDIR')),
            'native-host effective HOME/TMPDIR are not absolute')
    expected_environment={**static,'HOME':environment['HOME'],'TMPDIR':environment['TMPDIR'],
                          'CARGO_HOME':static.get('CARGO_HOME',str(Path(environment['HOME'])/'.cargo')),
                          'PWD':workspace,'CARGO_TARGET_DIR':workspace+'/.local/target',
                          'RUSTC':plan['tools']['rustc']['path'],'RUSTDOC':plan['tools']['rustdoc']['path'],
                          'RUSTC_WRAPPER':plan['rustc_wrapper']['path'],'CARGO_BUILD_BUILD_DIR':role,
                          'NUDOX_CARGO_BUILD_SLOTS':str(plan['build_slots'])}
    require(isinstance(environment,dict) and environment.get('CARGO_TARGET_DIR')==workspace+'/.local/target'
            and environment.get('CARGO_BUILD_BUILD_DIR')==proof.get('build_role')
            and environment.get('RUSTC') == tools['rustc']['path']
            and environment.get('RUSTDOC') == tools['rustdoc']['path']
            and environment.get('CARGO_HOME')==plan['cargo_home']
            and environment.get('NUDOX_CARGO_BUILD_SLOTS')==str(plan['build_slots'])
            and environment==expected_environment
            and all(isinstance(k,str) and isinstance(v,str) and not k.startswith('CARGO_PROFILE_') for k,v in environment.items()),
            'native-host effective environment differs')
    metadata=json.loads(_read_ref(receipt_path,refs['metadata'],8*1024**2))
    for name in PACKAGES:
        package=[p for p in metadata.get('packages',[]) if p.get('name')==name]
        require(len(package)==1 and package[0].get('manifest_path')==workspace+'/'+PACKAGE_ROOTS[name]+'/Cargo.toml',
                'native-host metadata package operand differs')
        targets=[t for t in package[0].get('targets',[]) if t.get('name')==name and t.get('kind')==['bin']]
        require(len(targets)==1 and targets[0].get('src_path')==workspace+'/'+PACKAGE_ROOTS[name]+'/src/main.rs',
                'native-host metadata bin operand differs')
    events = {};source_events=[]
    for line in _read_ref(receipt_path, refs['cargo_log'], MAX_LOG).splitlines():
        try: event = json.loads(line)
        except (ValueError, UnicodeDecodeError): continue
        if not isinstance(event,dict) or event.get('reason') != 'compiler-artifact':continue
        manifest=event.get('manifest_path')
        if isinstance(manifest,str) and Path(manifest).is_relative_to(workspace):
            require(Path(manifest).is_absolute() and '..' not in Path(manifest).parts and event.get('fresh') is False,
                    'native-host source-rooted artifact was reused without producing-source proof')
            source_events.append(event)
        name=event.get('target',{}).get('name')
        if name not in PACKAGES or not event.get('executable'):continue
        require(name not in events, 'duplicate native-host producing artifact')
        events[name]=event
    require(set(events) == set(PACKAGES) and proof.get('cargo_artifacts') == events,
            'native-host proof lacks exact raw producing artifacts')
    require(proof.get('source_artifacts')==source_events,
            'native-host complete source-rooted artifact inventory differs from raw events')
    outputs=proof.get('outputs'); require(isinstance(outputs,list) and len(outputs)==4,
            'native-host output count differs')
    output_by_name={item.get('path'):item for item in outputs}
    require(set(output_by_name)=={output_name(name) for name in PACKAGES},
            'native-host output identities differ')
    for name,event in events.items():
        profile=event.get('profile',{}); package=PACKAGE_ROOTS[name]
        require(event.get('target',{}).get('kind') == ['bin']
                and event['target'].get('src_path') == workspace+'/'+package+'/src/main.rs'
                and event.get('manifest_path') == workspace+'/'+package+'/Cargo.toml'
                and profile.get('opt_level') == '3' and profile.get('debuginfo') == 0
                and profile.get('debug_assertions') is False
                and profile.get('overflow_checks') is False and profile.get('test') is False
                and event.get('fresh') is False,
                'native-host artifact is not the exact optimized package/bin')
        path=Path(event['executable'])
        require(path == Path(workspace)/'.local/target/release'/name,
                'native-host artifact escaped selected native release target')
        item=output_by_name[output_name(name)]
        require(type(item.get('size_bytes')) is int and 0<item['size_bytes']<=MAX_IMAGE
                and item.get('cargo_executable')==str(path)
                and re.fullmatch('[0-9a-f]{64}',item.get('sha256','')) is not None,
                'native-host image size/output identity differs')


def build(args, api):
    """Run one new invocation; retain failed logs rather than inventing a receipt."""
    # These Git inspections do not invoke a pager. Repository/config overrides
    # must not redirect either the selected source identity or its blob walk.
    require(not any(name.startswith('GIT_') and name!='GIT_PAGER' for name in os.environ),
            'managed native-host source admission refuses ambient GIT_* overrides')
    source=args.source_root.resolve(strict=True);output=args.output_dir.resolve(strict=False)
    raw=read_regular_bytes(args.managed_native_host_plan,65536,'managed native-host plan')
    require(digest(raw)==args.expected_managed_plan_sha256,'managed native-host plan pin differs')
    plan=read_plan(raw)
    assets={'runner':args.expected_runner_sha256};paths={}
    for label,entry in plan_assets(plan):
        path=Path(entry['path']);_,actual=admit_file_digest(path,MAX_IMAGE)
        require(actual==entry['sha256'],'managed native-host asset bytes differ: '+label)
        assets['runner.'+label]=actual;paths[label]=str(path)
    runner_path=args.cargo_runner.resolve(strict=True)
    runner_raw=read_regular_bytes(runner_path,128*1024,'managed runner')
    require(digest(runner_raw)==args.expected_runner_sha256,'managed runner pin differs')
    first=runner_raw.splitlines()[0].decode()
    require(first=='#!'+paths['interpreter'],'managed runner interpreter differs')
    runner_cargo(runner_raw,plan['tools']['cargo']['path'])
    role=Path(plan['build_role']);require(role.is_absolute() and '..' not in role.parts
                                        and role.is_relative_to(source/'.local'),
                                        'managed native-host role must remain workspace owned')
    lifetime_locks=[]
    lock_identities={}
    try:
        for name in plan['lifetime_locks']:
            require(Path(name).is_absolute(), 'managed native-host lifetime lock is not absolute')
            fd=os.open(name,os.O_RDWR|os.O_NOFOLLOW|os.O_CLOEXEC)
            handle=os.fdopen(fd,'r+');lifetime_locks.append(handle);metadata=os.fstat(handle.fileno())
            require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid==os.geteuid() and metadata.st_nlink==1,
                    'managed native-host lifetime lock is not an owned persistent file')
            fcntl.flock(handle,fcntl.LOCK_EX|fcntl.LOCK_NB)
            lock_identities[name]=(metadata.st_dev,metadata.st_ino)
        owned_graph_directory(source,source/'.local/target')
        owned_graph_directory(source,source/'.local/target/release')
        owned_graph_directory(source,source/'.local/target/.nudox-provenance')
        owned_graph_directory(source,role)
        owned_graph_directory(source,role/'.nudox-cargo/leases')
        for index in range(min(4,plan['build_slots'])):
            owned_graph_directory(source,role/'.nudox-cargo'/('slot-'+str(index))/'release')
        environment=api['_static_environment'](Path(paths['environment.0']))
        require(environment.get('NUDOX_CARGO_BUILD_SLOTS',str(plan['build_slots']))==str(plan['build_slots']),
                'managed native-host selected environment slot count differs')
        tool_paths={name:plan['tools'][name]['path'] for name in plan['tools']}
        runner={'execution_kind':KIND,'path':str(runner_path),'sha256':args.expected_runner_sha256,'expected_sha256':args.expected_runner_sha256,
                'invocation_prefix':['$CARGO_RUNNER_INTERPRETER','$CARGO_RUNNER'],
                'referenced_asset_sha256':{**assets,'runner.exec':assets['runner.tool.CARGO']},
                '_environment':environment,'_tool_paths':tool_paths}
        env=api['_direct_build_environment'](runner,source,source/'.local/target')
        env.update(RUSTC_WRAPPER=paths['tool.RUSTC_WRAPPER'],CARGO_BUILD_BUILD_DIR=str(role),NUDOX_CARGO_BUILD_SLOTS=str(plan['build_slots']))
        require(env['CARGO_HOME']==plan['cargo_home'],'managed native-host effective Cargo home differs from plan')
        configs_before=capture_configs(source,plan)
        def tool_snapshot():
            result=api['_direct_tool_snapshot'](runner,source,env)
            result={name:{**value,'path':tool_paths[name]} for name,value in result.items()}
            vv=capture([tool_paths['rustc'],'-vV'],1024**2,cwd=source,env=env).decode()
            hosts=[line.removeprefix('host: ') for line in vv.splitlines() if line.startswith('host: ')]
            require(hosts==[args.target],'selected rustc host differs from requested native host')
            return {**result,'rustc_vv':vv,'host':hosts[0]}
        identity=source_identity(source,args.expected_revision,args.expected_tree)
        output.mkdir();evidence=output/'evidence';evidence.mkdir()
        refs={'plan':_raw_reference(evidence,'plan.json',raw)}
        refs['runner_script']=_raw_reference(evidence,'runner.sh',runner_raw)
        refs['environment_script']=_raw_reference(evidence,'environment.sh',read_regular_bytes(Path(paths['environment.0']),128*1024,'native-host environment script'))
        refs['configs_before']=_reference(evidence,'configs-before.json',configs_before)
        refs['environment']=_reference(evidence,'environment.json',env)
        metadata=capture([tool_paths['cargo'],'metadata','--locked','--offline','--no-deps','--format-version','1'],8*1024**2,cwd=source,env=env)
        refs['metadata']=_reference(evidence,'metadata.json',json.loads(metadata))
        refs['source_before']=_reference(evidence,'source-before.json',tracked_inputs(source,identity))
        refs['tools_before']=_reference(evidence,'tools-before.json',tool_snapshot())
        provenance_dir=source/'.local/target/.nudox-provenance';previous={p.name for p in provenance_dir.glob('*.json')}
        command=[paths['interpreter'],str(runner_path),*build_arguments()]
        try:
            owned_graph_directory(source,source/'.local/target')
            owned_graph_directory(source,source/'.local/target/release')
            owned_graph_directory(source,source/'.local/target/.nudox-provenance')
            owned_graph_directory(source,role)
            execution=api['_stream_direct_cargo'](command,source,env,evidence/'cargo.log',maximum_log_bytes=MAX_LOG)
            require(execution['returncode']==0,'managed native-host Cargo build failed; raw output retained')
            require(source_identity(source,args.expected_revision,args.expected_tree)==identity,
                    'managed native-host source identity changed')
            refs['source_after']=_reference(evidence,'source-after.json',tracked_inputs(source,identity))
            refs['configs_after']=_reference(evidence,'configs-after.json',capture_configs(source,plan))
            # Re-admit every original runner/environment/wrapper/tool pin after execution.
            for label,path in paths.items():require(admit_file_digest(Path(path),MAX_IMAGE)[1]==assets['runner.'+label],
                                                   'managed native-host referenced asset changed')
            require(digest(read_regular_bytes(runner_path,128*1024,'managed runner'))==args.expected_runner_sha256,'managed runner changed')
            refs['tools_after']=_reference(evidence,'tools-after.json',tool_snapshot())
            for name,expected in lock_identities.items():
                current=Path(name).lstat()
                require(stat.S_ISREG(current.st_mode) and current.st_nlink==1
                        and (current.st_dev,current.st_ino)==expected,
                        'managed native-host lifetime lock was replaced')
            wrapper=api['cargo_provenance'](provenance_dir,previous,source,identity,None,source/'.local/target')
            new_records={p.name:p for p in provenance_dir.glob('*.json') if p.name not in previous}
            require(len(new_records)==1,'managed wrapper record count changed')
            wrapper_raw=read_regular_bytes(next(iter(new_records.values())),8*1024**2,'native wrapper raw record')
            actual_build=Path(json.loads(wrapper_raw)['cargo_build_dir'])
            physical_graph={'target':owned_graph_directory(source,source/'.local/target',True),
                            'release':owned_graph_directory(source,source/'.local/target/release',True),
                            'provenance':owned_graph_directory(source,provenance_dir,True),
                            'build_role':owned_graph_directory(source,role,True),
                            'actual_build':owned_graph_directory(source,actual_build,True)}
            refs['graph_after']=_reference(evidence,'graph-after.json',physical_graph)
            (evidence/'wrapper.json').write_bytes(wrapper_raw)
            refs['wrapper']={'path':'evidence/wrapper.json','sha256':digest(wrapper_raw),'size_bytes':len(wrapper_raw)}
            refs['cargo_log']={'path':'evidence/cargo.log',**{k:execution['output_log'][k] for k in ('sha256','size_bytes')}}
            refs['execution']=_reference(evidence,'execution.json',{
                **{key:execution[key] for key in ('returncode','child_pid','started_at_utc','finished_at_utc','elapsed_ns','output_log')},
                'command':command,'cwd':str(source),'retirement':'owned-child-kernel-wait'})
            events={};source_events=[]
            for line in read_regular_bytes(evidence/'cargo.log',MAX_LOG,'native Cargo log').splitlines():
                try:event=json.loads(line)
                except (ValueError,UnicodeDecodeError):continue
                if isinstance(event,dict) and event.get('reason')=='compiler-artifact' and isinstance(event.get('manifest_path'),str) and Path(event['manifest_path']).is_relative_to(source):source_events.append(event)
                if isinstance(event,dict) and event.get('reason')=='compiler-artifact' and event.get('executable') and event.get('target',{}).get('name') in PACKAGES:
                    name=event['target']['name'];require(name not in events,'duplicate native producing event');events[name]=event
            outputs=[]
            for name,event in events.items():
                owned_graph_directory(source,Path(event['executable']).parent,True)
                size,sha=admit_file_digest(Path(event['executable']),MAX_IMAGE)
                outputs.append({'path':output_name(name),'cargo_executable':event['executable'],'sha256':sha,'size_bytes':size})
            proof={'schema':SCHEMA,'kind':KIND,'target_mode':'native-host','target':args.target,'workspace_root':str(source),
                   'build_role':str(role),'command':[*runner['invocation_prefix'],*build_arguments()],
                   'operator_plan_sha256':digest(raw),
                   'physical_graph_directories':physical_graph,
                   'retirement':'owned-child-kernel-wait','child_pid':execution['child_pid'],'exit_status':0,
                   'raw_evidence':refs,'managed_wrapper_provenance':wrapper,'cargo_artifacts':events,'source_artifacts':source_events,'outputs':outputs}
            proof['record_sha256']=proof_digest(proof)
            public_runner={k:v for k,v in runner.items() if not k.startswith('_')}
            validate(proof,identity,args.target,public_runner,output/'application-build-receipt.json',args.expected_managed_plan_sha256)
            artifacts=output/'artifacts';artifacts.mkdir()
            for item in outputs:
                name=Path(item['cargo_executable']).name;dest=artifacts/name;shutil.copy2(item['cargo_executable'],dest)
                require(admit_file_digest(dest,MAX_IMAGE)==(item['size_bytes'],item['sha256']), 'frozen image copy differs')
            receipt={'schema':1,'source':identity,'source_before':identity,'source_after':identity,'source_unchanged':True,
                     'target':args.target,'profile':'release','locked_build':True,'cargo_runner_before':public_runner,
                     'cargo_runner_after':public_runner,'cargo_runner_unchanged':True,'cargo_provenance':proof,
                     'command':proof['command'],'executables':{Path(i['cargo_executable']).name:i['sha256'] for i in outputs}}
            (output/'application-build-receipt.json').write_bytes(canonical(receipt))
        except BaseException as error:
            (evidence/'failure.json').write_bytes(canonical({'type':type(error).__name__,'message':str(error),
                                                          'application_build_receipt_emitted':False}))
            raise
        return 0
    finally:
        for handle in reversed(lifetime_locks):handle.close()
