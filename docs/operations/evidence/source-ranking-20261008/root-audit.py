import argparse, hashlib, json, re, shutil, subprocess
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--repo', required=True)
p.add_argument('--output', required=True)
p.add_argument('--evidence', required=True)
a = p.parse_args()
repo, dest = Path(a.repo), Path(a.evidence)
dest.mkdir(parents=True, exist_ok=True)
def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()
def read(path):
    return json.loads(Path(path).read_text())
def git(*args):
    return subprocess.check_output(['git', *args], cwd=repo)
checks, retained = [], {}
def keep(path, relative):
    path = Path(path)
    target = dest / relative
    target.parent.mkdir(parents=True, exist_ok=True)
    if target.exists():
        assert target.read_bytes() == path.read_bytes(), target
    else:
        shutil.copyfile(path, target)
    retained[relative] = {'sha256': sha(target), 'bytes': target.stat().st_size,
                          'original_path': str(path)}
    return target
proof_path = Path('/private/tmp/sol61-search-source-ranking-native-proof.json')
assert sha(proof_path) == 'c161970149bc6bd51b3094ad015947492e33f663ecf6c45c3b927f687fd667a3'
proof = read(keep(proof_path, 'native-proof.json'))
source = read(keep(proof['source_packet'], 'source-map.json'))
assert sha(proof['source_packet']) == proof['source_packet_sha256']
keep(proof['plan'], 'native-plan.json')
assert sha(proof['plan']) == proof['plan_sha256']
native = proof['native_source']
integration = git('rev-parse', 'HEAD').decode().strip()
for f in source['files']:
    for ref in [source['head'], native, integration]:
        data = git('show', ref + ':' + f['path'])
        blob = git('rev-parse', ref + ':' + f['path']).decode().strip()
        assert blob == f['blob'] and hashlib.sha256(data).hexdigest() == f['sha256'], (ref, f['path'])
    checks.append({'path': f['path'], 'all_three_blobs_equal': True, 'sha256': f['sha256']})
for f in source['artifacts']:
    assert sha(f['path']) == f['sha256']
    if f['path'].endswith('.patch'):
        keep(f['path'], 'original-source.patch')
summaries = []
for gate in proof['gates']:
    root = Path(gate['completion']).parent
    completion = read(root / 'completion.json')
    launch = read(root / 'launch.json')
    fleet = read(root / 'fleet.json')
    assert sha(root / 'completion.json') == gate['completion_sha256']
    assert sha(root / 'launch.json') == gate['launch_sha256']
    assert sha(root / 'fleet.json') == gate['fleet_sha256']
    assert sha(root.parent / 'orchestration.json') == gate['orchestration_sha256']
    for name, expected in completion['artifact_hashes'].items():
        assert sha(root / name) == expected, name
    for path in root.parent.rglob('*'):
        if path.is_file():
            keep(path, 'gate%d/%s' % (gate['gate'], path.relative_to(root.parent)))
    assert completion['exit'] == 0 and completion['compiler_errors'] == 0
    assert completion['source_after'] == launch['source_before'] == gate['source']
    for field in ['source_unchanged', 'frozen_graph_unchanged', 'environment_unchanged',
                  'toolchain_unchanged', 'managed_permit_retired', 'managed_graph_leases_empty',
                  'binary_result_mapping_complete']:
        assert completion[field] is True, field
    assert completion['retirement'] == 'owned-child-kernel-wait'
    assert launch['argv'] == gate['argv'] and launch['plan_sha256'] == proof['plan_sha256']
    assert fleet['advisory_allowed'] and not fleet['reasons']
    assert fleet['current_fleet_compiler_admission_slot_count'] < 16
    assert fleet['hosts']['h16001mac']['compiler_admission_slot_count'] < 8
    assert launch['admission']['age_at_validation_seconds'] <= 30
    assert launch['admission']['destination_available_memory_bytes'] >= 8 * 1024**3
    assert launch['admission']['destination_root_disk_available_bytes'] >= 16 * 1024**3
    assert all(h['complete'] and isinstance(h['available_memory_bytes'], int) for h in fleet['hosts'].values())
    rows = []
    for line in (root / 'cargo.stdout.jsonl').read_text().splitlines():
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    images = [r for r in rows if r.get('reason') == 'compiler-artifact' and r.get('executable')
              and r.get('target', {}).get('name') == 'backend_local_service' and r.get('profile', {}).get('test')]
    assert len(images) == 1 and len(completion['native_executables']) == 1
    image, sealed = images[0], completion['native_executables'][0]
    assert image['executable'] == sealed['path'] and image['target'] == sealed['target']
    assert sealed['sha256'] == proof['binary_sha256'] == gate['binary_sha256']
    assert image['fresh'] == sealed['cargo_fresh'] == gate['cargo_fresh']
    text = (root / 'cargo.stderr.log').read_text() + (root / 'cargo.stdout.jsonl').read_text()
    results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', text)
    assert results == [(str(gate['expected_passed']), '0', '0')], results
    summaries.append({'gate': gate['gate'], 'passed': gate['expected_passed'],
                      'cargo_artifact_bound': True, 'binary_sha256': sealed['sha256']})
for prior in proof['preserved_prior_attempts']:
    directory = Path(prior['directory'])
    for path in directory.rglob('*'):
        if path.is_file():
            keep(path, 'prior/%s/%s' % (directory.name, path.relative_to(directory)))
    assert sha(directory / 'orchestration.json') == prior['orchestration_sha256']
    if prior['completion_sha256']:
        assert sha(directory / 'native/completion.json') == prior['completion_sha256']
returned = keep('/private/tmp/luna-index-projection-graph-return-after-ranking-20261008.json', 'graph-return.json')
assert sha(returned) == '40dd43425d8307e72a653f47bf31ec57d3ab964f0923397cc7812e96bfcb4b24'
return_record = read(returned)
assert return_record['graph_source_unchanged'] and return_record['managed_graph_leases_before_after'] == [[], []]
assert return_record['active_target_compiler_processes_at_return'] == []
keep('/private/tmp/sol61-search-ranking-native-234a-owner-archive-before-final-return-20261008.json', 'owner-before-return.json')
assert sha(dest / 'owner-before-return.json') == return_record['previous_owner']['archive_sha256']
base = '82886bd75348eb58ca2522ac0cd82f3f9cbd529b'
changed = set(git('diff', '--name-only', base, integration).decode().splitlines())
assert changed == {f['path'] for f in source['files']}, changed
assert git('show', base + ':Cargo.lock') == git('show', integration + ':Cargo.lock')
report = {'schema': 'nudox.root-source-ranking-audit.v1', 'integration_source': integration,
          'integration_base': base, 'native_source': native, 'source_files': checks,
          'gates': summaries, 'passed': sum(g['passed'] for g in summaries), 'failed': 0,
          'original_failed_and_denied_attempts_preserved': len(proof['preserved_prior_attempts']),
          'retained_files': retained, 'scope': proof['scope'],
          'remote_binary_rehashed_by_root': False,
          'note': 'Root validates actual Cargo event/receipt binding, not a locally available native image. The remote image hash is a supervisor observation; no installed-release proof.'}
Path(a.output).write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({'passed': report['passed'], 'failed': 0, 'source_files': len(checks),
                  'retained_files': len(retained), 'output': a.output}))
