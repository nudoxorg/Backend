#!/usr/bin/env python3
"""Independently verify focused raw results, preserving artifact limitations."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

ROOT = Path('/private/tmp/nudox-index-projection-checkpoint-20261008')
MANIFEST = Path('/private/tmp/luna-index-projection-fence7382-evidence-manifest.json')

def load(path):
    return json.loads(path.read_bytes())

def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args])

assert digest(MANIFEST) == 'fdda0423d5a534c6b5217f9af422222555c160e675af2814b8d1347a4253855c'
manifest = load(MANIFEST)
candidate = manifest['plan']['source']['commit']
assert git('rev-parse', candidate+'^{tree}').decode().strip() == manifest['plan']['source']['tree']
paths = git('diff', '--name-only', '52bee7f1b0', candidate).decode().splitlines()
assert len(paths) == 13
assert paths == git('diff', '--name-only', 'b78b4ded9b', 'HEAD').decode().splitlines()
for path in paths:
    assert git('show', candidate+':'+path) == git('show', 'HEAD:'+path), path
assert git('show', 'HEAD:Cargo.lock') == git('show', candidate+':Cargo.lock')

gates = []
for gate in manifest['gates']:
    folder = Path(gate['local_archive_directory']) / Path(gate['remote_output_directory']).name
    completion_path = folder/'completion.json'
    assert digest(completion_path) == gate['completion_sha256']
    completion = load(completion_path)
    for name, expected in completion['artifact_hashes'].items():
        assert digest(folder/name) == expected, name
    assert completion['exit'] == 0 and completion['stop_reason'] is None
    for key in ['source_unchanged', 'frozen_graph_unchanged', 'environment_unchanged',
                'toolchain_unchanged', 'managed_permit_retired', 'managed_graph_leases_empty']:
        assert completion[key], key
    assert completion['retirement'] == 'owned-child-kernel-wait'
    assert completion['binary_result_mapping_complete'] is False
    assert completion['native_executables'] == []
    launch = load(folder/'launch.json')
    assert launch['source_before'] == completion['source_after']
    assert launch['source_before']['commit'] == candidate
    assert launch['frozen_graph_before'] == completion['frozen_graph_after']
    assert launch['argv'][1:6] == ['test','--verbose','--locked','--offline','-j2']
    assert gate['test'] in launch['argv']
    assert 0 <= launch['admission']['age_at_validation_seconds'] <= 30
    assert launch['admission']['fleet_groups'] < 16
    assert launch['admission']['destination_groups'] < 8
    assert launch['admission']['destination_available_memory_bytes'] >= 8*1024**3
    assert launch['admission']['destination_root_disk_available_bytes'] >= 16*1024**3
    stdout = (folder/'cargo.stdout.jsonl').read_text()
    stderr = (folder/'cargo.stderr.log').read_text()
    assert re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', stdout) == [('1','0','0')]
    assert len(re.findall(r'^test .*'+re.escape(gate['test'])+r' \.\.\. ok$', stdout, re.M)) == 1
    proof_path = Path(gate['proof_path'])
    assert digest(proof_path) == gate['proof_sha256']
    proof = load(proof_path)
    assert proof['cargo_verbose_running_line'] in stderr
    assert gate['test'] in proof['cargo_verbose_running_line']
    if gate['gate'] == 2:
        assert proof['executed_parent_test_binary'] == gate['test_binary']
        assert proof['nested_worker_filter_alone_was_not_counted'] is True
        declared_source = proof['parent_test_source']
        assert hashlib.sha256(git('show',candidate+':'+declared_source['path'])).hexdigest() == declared_source['sha256']
    else:
        assert proof['executed_test_binary'] == gate['test_binary']
    assert proof['completion_sha256'] == digest(completion_path)
    assert proof['source_before'] == launch['source_before']
    gates.append({'gate':gate['gate'], 'test':gate['test'], 'passed':1,
                  'raw_completion_sha256':digest(completion_path),
                  'binary_hash_from_supplement_not_independently_rehashed':gate['test_binary']['sha256']})

retirement = Path(manifest['final_retirement_check']['path'])
assert digest(retirement) == manifest['final_retirement_check']['sha256']
report = {'schema':'nudox.root.index-projection.focused-audit.v1',
          'integrated_source':git('rev-parse','HEAD').decode().strip(),
          'candidate_source':candidate, 'exact_changed_paths_verified':paths,
          'native_gates':gates,'actual_focused_passed':4,
          'raw_supervisor_binary_mapping_complete':False,
          'supplemental_verbose_invocations_verified':True,
          'test_images_copied_or_independently_hashed_locally':False,
          'whole_workspace_pass_inferred':False,'installed_public_binaries_updated':False}
destination = Path('/private/tmp/nudox-root-index-projection-audit-20261008.json')
destination.write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
