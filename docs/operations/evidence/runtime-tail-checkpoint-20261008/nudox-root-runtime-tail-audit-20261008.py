#!/usr/bin/env python3
"""Independently audit frozen focused runs; never infer public acceptance."""
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile

REPO = Path('/private/tmp/nudox-runtime-tail-checkpoint-20261008')
GO = Path('/Users/mileswirht/Downloads/sol61-go-capture-scratch-native-6c5853-20261008')
CAS = Path('/private/tmp/sol61-docs-add-profile-20261008/native-01-proof')
TS = Path('/Users/mileswirht/Downloads/sol61-journal-public-recovery-20261008/excal-native12-raw-evidence')

def sha(data):
    return hashlib.sha256(data).hexdigest()

def file_sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()

def load(path):
    return json.loads(path.read_bytes())

def git(*args):
    return subprocess.check_output(['git', '-C', str(REPO), *args])

def verify_archive(path, expected, directory):
    assert file_sha(path) == expected, path
    names = set()
    with tarfile.open(path) as archive:
        for member in archive:
            assert member.name not in names, member.name
            names.add(member.name)
            relative = Path(member.name)
            assert not relative.is_absolute() and '..' not in relative.parts
            if member.isdir():
                continue
            assert member.isfile(), member.name
            target = directory / relative
            assert target.is_file() and not target.is_symlink(), target
            with archive.extractfile(member) as source:
                assert hashlib.file_digest(source, 'sha256').hexdigest() == file_sha(target), target
            assert target.stat().st_size == member.size
    return len(names)

def test_summary(text):
    return [tuple(map(int, item)) for item in re.findall(
        r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)]

def assert_blobs(ref, paths):
    for path in paths:
        assert git('show', f'{ref}:{path}') == git('show', f'HEAD:{path}'), path

go_packet = load(Path('/private/tmp/sol61-go-capture-scratch-native-source-packet-20261008.json'))
go_archive = go_packet['raw_archive']
go_members = verify_archive(Path(go_archive['path']), go_archive['sha256'], GO)
seal_path = GO / 'scratch-6c5853-native-seal-01/receipt.json'
assert file_sha(seal_path) == go_packet['native_seal']['sha256']
seal = load(seal_path)
assert seal['all_children_kernel_retired'] and seal['managed_leases_empty'] and seal['stamps_unchanged']
assert not seal['release_graph_used'] and not seal['public_cli_mcp_acceptance_inferred']
assert seal['source']['commit'] == '6c58535f63fd31d65aafe18ae4219077753f6f07'
go_runs = []
inputs = set()
generated_inputs = set()
images = set()
for run in seal['runs']:
    folder = GO / Path(run['receipt']).parent.name
    receipt_path = folder / 'receipt.json'
    assert file_sha(receipt_path) == run['receipt_sha256']
    receipt = load(receipt_path)
    assert receipt['exit'] == 0 and not receipt['timed_out']
    assert receipt['children_kernel_retired'] and not receipt['survivors']
    assert receipt['managed_leases_empty'] and receipt['stamps_unchanged']
    assert receipt['source'] == seal['source']
    for name, key in [('cargo.stdout', 'stdout_sha256'), ('cargo.stderr', 'stderr_sha256')]:
        assert file_sha(folder / name) == run[key] == receipt[key]
    assert file_sha(folder / 'launch.json') == run['launch_sha256']
    text = (folder / 'cargo.stdout').read_text()
    assert test_summary(text) == [(run['passed'], 0, 0)]
    assert len(re.findall(r'^test .* \.\.\. ok$', text, re.M)) == run['passed']
    for image in run['artifacts']:
        path = GO / 'scratch-6c5853-native-seal-01/images' / Path(image['sealed_copy']).name
        assert file_sha(path) == image['sha256'] and path.stat().st_size == image['bytes']
        images.add(image['sha256'])
    for original, source in run['compiler_inputs'].items():
        path = GO / 'scratch-6c5853-native-seal-01/compiler-inputs' / Path(source['sealed_copy']).name
        assert file_sha(path) == source['sha256'] and path.stat().st_size == source['bytes']
        inputs.add(original)
        prefix = '/root/codex-worktrees/sol61-go-rust-public-tails-20261008/'
        if original.startswith(prefix):
            relative = original.removeprefix(prefix)
            if relative.startswith('.local/'):
                assert relative.endswith('/out/go_oracle_sources.rs'), relative
                generated_inputs.add(original)
            else:
                assert sha(git('show', f"{seal['source']['commit']}:{relative}")) == source['sha256']
    go_runs.append({'phase':run['phase'], 'passed':run['passed'], 'elapsed_seconds':run['elapsed_seconds']})
assert sum(run['passed'] for run in go_runs) == 20
go_paths = ['frontends/go/src/legacy/oracle.rs', 'frontends/go/src/legacy/oracle/authority_witness.rs',
            'frontends/go/src/legacy/oracle/capture_digest.rs', 'frontends/go/src/legacy/oracle/dependency_witness.rs']
assert_blobs(seal['source']['commit'], go_paths)

cas_members = verify_archive(CAS.parent / 'native-01-proof.tar.gz',
    'be2b5c003fe1d3bd83f660cfc92a9e67c1b6bbde5ab59f324f962c7750306119', CAS)
cas_path = CAS / 'native-01/receipt.json'
assert file_sha(cas_path) == '8e3e97ab37cba2134ed4b6326278394a7a13b26e9bfc438bb0a673ca3dc0c96f'
cas = load(cas_path)
assert cas['exit_code'] == 0 and cas['retirement'] == 'owned-child-kernel-wait'
assert not cas['remaining_group_after_retirement'] and cas['managed_leases_empty'] and cas['original_stamps_restored']
assert cas['source_before'] == cas['source_after'] and cas['owner_before'] == cas['owner_after']
assert cas['compiler_source_bound'] and cas['rustc_processes']
for suffix in ['stdout', 'stderr']:
    assert file_sha(CAS / f'native-01/{suffix}.log') == cas[f'{suffix}_sha256']
assert test_summary((CAS / 'native-01/stdout.log').read_text()) == [(4,0,0)]
for image in cas['artifacts']:
    path = CAS / 'native-01' / Path(image['frozen_path']).name
    assert file_sha(path) == image['sha256'] and path.stat().st_size == image['bytes']
cas_inputs = load(CAS / 'native-01/source-inputs.json')
for path, expected in cas_inputs.items():
    assert sha(git('show', f"{cas['source_before']['commit']}:{path}")) == expected, path
cas_paths = ['crates/store/src/durable/objects.rs', 'crates/store/src/durable/artifact_fs.rs']
assert_blobs(cas['source_before']['commit'], cas_paths)

ts_members = verify_archive(TS.with_suffix('.tar'),
    '9a0a3f6621209021270dbd38a430fd0c28c410dc3df519178017b7c874c2ae88', TS)
ts = load(TS / 'receipt.json')
assert ts['exit'] == 0 and ts['retirement'] == 'owned-child-kernel-wait'
assert ts['source_before'] == ts['source_after'] and ts['tool_sha256_before'] == ts['tool_sha256']
assert test_summary((TS / 'cargo.log').read_text()) == [(4,0,0)]
ts_inputs = load(TS / 'compiler-inputs.json')
assert file_sha(TS / 'compiler-inputs.json') == ts['compiler_inputs_sha256']
assert git('ls-tree', '-r', ts['source_before']['commit']).decode().splitlines() == ts_inputs['tracked_tree_entries']
for path, expected in ts_inputs['raw_sha256'].items():
    assert sha(git('show', f"{ts['source_before']['commit']}:{path}")) == expected
assert_blobs(ts['source_before']['commit'], ['crates/engine/src/application/compiler.rs'])

changed = git('diff','--name-only','52bee7f1b0b46db1ee7d07f62aaefa69c8f51c30','HEAD').decode().splitlines()
assert set(changed) == set(go_paths + cas_paths + ['crates/engine/src/application/compiler.rs'])
report = {'schema':'nudox.root-focused-runtime-tail-audit.v1', 'source_head':git('rev-parse','HEAD').decode().strip(),
          'source_tree':git('rev-parse','HEAD^{tree}').decode().strip(),
          'cargo_lock_sha256':sha(git('show','HEAD:Cargo.lock')), 'exact_changed_source_paths':changed,
          'go':{'runs':go_runs,'total_passed':20,'archive_members_verified':go_members,
                'actual_image_hashes_verified':sorted(images),'actual_compiler_inputs_verified':len(inputs),
                'generated_inputs_hash_verified_not_git_tracked':sorted(generated_inputs)},
          'cas':{'passed':4,'archive_members_verified':cas_members,'source_input_hashes_verified':len(cas_inputs),
                 'actual_test_image_verified':cas['artifacts'][0]['sha256']},
          'typescript_diagnostics':{'passed':4,'archive_members_verified':ts_members,
                 'tracked_source_tree_entries_verified':len(ts_inputs['tracked_tree_entries']),
                 'sealed_engine_image_not_copied_locally':True},
          'total_focused_passed':28,'public_runtime_acceptance_inferred':False,
          'combined_entire_workspace_test_run':False,'installed_binaries_updated':False}
destination = Path('/private/tmp/nudox-root-runtime-tail-audit-20261008.json')
destination.write_text(json.dumps(report,indent=2)+'\n')
print(json.dumps(report,indent=2))
