import hashlib
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path('/private/tmp/nudox-go-rust-checkpoint-20261008')
RAW = pathlib.Path('/Users/mileswirht/Downloads/sol61-go-rust-native-evidence-a430-20261008')
REMEDY = pathlib.Path('/Users/mileswirht/Downloads/sol61-go-remedy-native-10ab-20261008')
BASE = '8c18a3d29b641a10a697c160d7e1fb1d35661ae6'
TESTED = '10abbe5bc20524817a297716763940f754307830'
REPLAY = '518262c0de4c33a7788f481a3af65d29865f568b'

def sha(data):
    return hashlib.sha256(data).hexdigest()

def git(*args):
    return subprocess.check_output(['git', '-C', str(ROOT), *args])

def read(path):
    return json.loads(path.read_bytes())

def source(source):
    assert source['clean'] is True
    assert git('rev-parse', source['commit'] + '^{tree}').decode().strip() == source['tree']
    assert sha(git('show', source['commit'] + ':Cargo.lock')) == source['cargo_lock_sha256']

checks = []
seal = read(RAW / 'native-evidence-seal-04a4.json')
for run in seal['runs']:
    if run.get('not_run'):
        checks.append({'phase': run['phase'], 'outcome': 'UNRUN', 'reason': run['reason']})
        continue
    directory = RAW / 'release-loan-2110' / run['phase']
    for filename, field in [('receipt.json', 'receipt_sha256'), ('launch.json', 'launch_sha256'), ('cargo.stdout', 'stdout_sha256'), ('cargo.stderr', 'stderr_sha256')]:
        assert sha((directory / filename).read_bytes()) == run[field], (directory, filename)
    receipt = read(directory / 'receipt.json')
    launch = read(directory / 'launch.json')
    source(receipt['source'])
    assert receipt['source'] == launch['source'] == run['source']
    assert receipt['children_kernel_retired'] and receipt['stamps_unchanged'] and not receipt['survivors']
    assert receipt['exit'] == run['exit']
    assert launch['admission']['advisory_allowed'] is True
    assert max(v['age_seconds'] for v in launch['admission']['hosts'].values()) <= 30
    assert launch['admission']['requested_cargo_jobs'] <= 4
    assert launch['admission']['hosts']['ilo']['available_memory_bytes'] >= 8 * 1024**3
    stdout = (directory / 'cargo.stdout').read_text()
    results = re.findall(r'^test (.+) \.\.\. (ok|FAILED|ignored.*)$', stdout, re.M)
    summary = re.findall(r'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored;', stdout)
    if run['phase'] in ['rust-session-01', 'rust-types-01', 'library-go-01']:
        expected = 22 if run['phase'] == 'library-go-01' else 1
        assert summary == [(str(expected), '0', '0')]
        assert len(results) == expected and all(result == 'ok' for _, result in results)
        assert len({name for name, _ in results}) == expected
    if run['phase'] == 'serde-native-02':
        phases = [json.loads(line) for line in stdout.splitlines() if line.startswith('{')]
        assert [phase['phase'] for phase in phases] == ['initial-online', 'cold-offline']
        for phase in phases:
            assert phase['declaration_count'] == 90 and phase['caller_declaration_count'] == 309
            assert phase['project_lockfile_absent'] and phase['selected_source_unchanged']
            assert len(phase['resolved_exact_trait_bound_offsets']) == 33
            assert phase['resolved_exact_typed_method_calls'][0][0] == 718
            assert len(phase['resolved_exact_typed_method_calls']) == 1
        assert receipt['real_app_source_unchanged']
    if run['phase'] in ['serde-native-01', 'present-go-01']:
        assert not results and not run['executables']
        assert receipt['exit'] in [101, 143]
    checks.append({'phase': run['phase'], 'exit': receipt['exit'], 'test_summary': summary, 'result_names': [name for name, _ in results], 'source': receipt['source'], 'elapsed_seconds': receipt['elapsed_seconds'], 'raw_hashes_verified': True})

for directory, expected in [(RAW / 'focused-tests-09-go-selected-01', 1), (REMEDY / 'focused-tests-10-go-remedy-01', 8)]:
    receipt = read(directory / 'receipt.json')
    launch = read(directory / 'launch.json')
    source(receipt['source'])
    assert launch['source'] == receipt['source']
    assert receipt['exit'] == 0 and not receipt['timed_out']
    assert receipt['children_kernel_retired'] and receipt['managed_leases_empty'] and not receipt['survivors']
    for filename, field in [('cargo.stdout', 'stdout_sha256'), ('cargo.stderr', 'stderr_sha256')]:
        assert sha((directory / filename).read_bytes()) == receipt[field]
    stdout = (directory / 'cargo.stdout').read_text()
    results = re.findall(r'^test (.+) \.\.\. (ok|FAILED|ignored.*)$', stdout, re.M)
    assert re.findall(r'test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored;', stdout) == [(str(expected), '0', '0')]
    assert len(results) == expected and all(result == 'ok' for _, result in results)
    assert len({name for name, _ in results}) == expected
    if expected == 1:
        phases = [json.loads(line) for line in stdout.splitlines() if line.startswith('{')]
        assert [phase['phase'] for phase in phases] == ['same-owner-after-setup', 'cold-owner-offline']
        for phase in phases:
            assert phase['known_call_span'] == [86, 91] and phase['exact_owner_foreign_call_count'] == 1
            assert phase['declaration_count'] == 2 and phase['reference_count'] == 4
            assert phase['inferred_dependency_result_type'] == 'int'
    checks.append({'phase': directory.name, 'source': receipt['source'], 'passed': expected, 'result_names': [name for name, _ in results], 'raw_hashes_verified': True, 'receipt_sha256': sha((directory / 'receipt.json').read_bytes())})

returned = read(RAW / 'native-graph-return-04a4.json')
assert returned['reservation_returned'] and returned['managed_leases_empty'] and not returned['foreign_signals']
assert len(returned['groups_absent']) == 6
assert all(stamp['exact_original_bytes_restored'] for stamp in returned['stamps'])
assert git('rev-parse', 'HEAD^{tree}') == git('rev-parse', REPLAY + '^{tree}')
assert sha(git('show', 'HEAD:Cargo.lock')) == sha(git('show', BASE + ':Cargo.lock'))
changed = git('diff', '--name-only', BASE, 'HEAD').decode().splitlines()
assert len(changed) == 18
assert set(changed) == set(git('diff', '--name-only', '8809eb34a5c44ee83145c143b9d444124542adc8', TESTED).decode().splitlines())

result = {'schema': 'nudox.root-go-rust-native-review.v1', 'checks': checks, 'production_read_by_root': True, 'test_successors_read_by_root': True, 'integrated_source': git('rev-parse', 'HEAD').decode().strip(), 'tree': git('rev-parse', 'HEAD^{tree}').decode().strip(), 'base': BASE, 'replay_tree_identical': True, 'changed_paths': changed, 'canonical_lock_unchanged': True, 'public_cli_mcp_acceptance': False, 'limitations': ['Native checks run by worker on exact recorded sources; Root independently audited raw output, hashes, source trees and retirement.', 'Canonical replay is source-only; engine and present Go test gates remain unrun/timed out as recorded.', 'No current installed CLI/MCP or GUI acceptance is inferred from frontend tests.']}
output = pathlib.Path('/private/tmp/nudox-root-go-rust-native-audit-20261008.json')
output.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'audit': str(output), 'sha256': sha(output.read_bytes()), 'tree': result['tree'], 'checks': len(checks), 'actual_unique_test_results': sum(len(check.get('result_names', [])) for check in checks)}))
