import datetime
import hashlib
import json
import pathlib
import re
import subprocess
import tarfile

ROOT = pathlib.Path('/private/tmp/sol61-baseline-native-raw-review-146c-20261008')
REPO = '/private/tmp/nudox-python-native-checkpoint-20261008'
HEAD = '146c430f842df6042d62dc82c3eea4e9d77cf006'
manifest = json.loads((ROOT / 'manifest.json').read_text())

def sha(data):
    return hashlib.sha256(data).hexdigest()

def git(*args):
    return subprocess.check_output(['git', *args], cwd=REPO)

archive = pathlib.Path(manifest['raw_archive']['path'])
assert sha(archive.read_bytes()) == manifest['raw_archive']['sha256']
with tarfile.open(archive) as tar:
    for member in tar.getmembers():
        assert not member.name.startswith('/') and '..' not in pathlib.PurePosixPath(member.name).parts
        assert member.isfile() or member.isdir()

results = []
for gate in manifest['gates']:
    name = gate['gate']
    directory = ROOT / name
    receipt = json.loads((directory / 'receipt.json').read_text())
    for file in gate['files']:
        data = pathlib.Path(file['path']).read_bytes()
        assert len(data) == file['bytes'] and sha(data) == file['sha256']
    assert receipt['exit_code'] == 0
    assert receipt['source_before'] == receipt['source_after'] == gate['tested_source']
    assert receipt['source_before']['clean']
    assert receipt['retirement'] == 'owned-child-kernel-wait'
    commit = receipt['source_before']['commit']
    assert git('rev-parse', commit + '^{tree}').decode().strip() == receipt['source_before']['tree']
    assert sha(git('show', commit + ':Cargo.lock')) == receipt['source_before']['lock_sha256']
    assert not git('diff', '--name-only', commit, HEAD, '--', *gate['gate_scope']).strip()
    started = datetime.datetime.fromisoformat(receipt['started_utc'])
    census = datetime.datetime.fromisoformat(receipt['fleet']['collector_started_at_utc'])
    assert 0 <= (started - census).total_seconds() <= 30
    assert receipt['fleet']['advisory_allowed']
    stdout = (directory / 'cargo.stdout').read_bytes()
    stderr = (directory / 'cargo.stderr').read_bytes()
    assert sha(stdout) == receipt['hashes']['cargo.stdout']
    assert sha(stderr) == receipt['hashes']['cargo.stderr']
    raw = stdout.decode() + '\n' + stderr.decode()
    summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;[^\n]*', raw)
    assert len(summaries) == 1, (name, summaries)
    passed, failed, ignored = map(int, summaries[0])
    assert failed == ignored == 0
    individual = re.findall(r'^test .+ \.\.\. ok$', raw, re.M)
    assert len(individual) == passed, (name, len(individual), passed)
    artifacts, errors = [], []
    for line in stdout.decode().splitlines():
        if not line.startswith('{'):
            continue
        event = json.loads(line)
        if event.get('reason') == 'compiler-message' and event['message'].get('level') == 'error':
            errors.append(event)
        if event.get('reason') == 'compiler-artifact' and event.get('profile', {}).get('test'):
            artifacts.append({'target': event['target']['name'], 'executable': event.get('executable'),
                              'fresh': event['fresh'], 'source': event['target']['src_path']})
    assert not errors and any(a['executable'] for a in artifacts)
    results.append({'gate': name, 'source': commit, 'passed': passed, 'failed': failed,
                    'ignored': ignored, 'elapsed_seconds': receipt['elapsed_seconds'],
                    'scope_identical_at_final_head': gate['gate_scope'], 'artifacts': artifacts})

prefix = 'crates/present/fixtures/'
selected_bytes = git('show', HEAD + ':' + prefix + 'worst-200-full-records-selected-id.json')
selected = json.loads(selected_bytes)
historical = json.loads(git('show', HEAD + ':' + prefix + 'worst-200-full-records.json'))
assert len(selected_bytes) == 134052
assert len(selected['records']) == 200
for row in selected['records']:
    value = row['identity'].pop('semantic_data')
    assert value['kind'] == 'selected-symbol-id'
    assert len(value['value']) == 32 and all(isinstance(x, int) and 0 <= x <= 255 for x in value['value'])
selected['budget'] = historical['budget']
assert selected == historical

result = {'source': HEAD, 'raw_archive_sha256': sha(archive.read_bytes()), 'gates': results,
          'total_test_passes': sum(r['passed'] for r in results),
          'golden': {'bytes': len(selected_bytes), 'records': 200, 'historical_fields_preserved': True},
          'limits': ['Successive source cohorts, not a final combined native run.',
                     'Artifact records and kernel-wait receipts audited; remote images not independently rehashed here.',
                     'Real installed CLI/MCP restart/interleaving replay remains pending.']}
out = pathlib.Path('/private/tmp/nudox-root-baseline146c-audit-20261008.json')
out.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({'audit': str(out), 'gates': len(results), 'test_passes': result['total_test_passes'],
                  'golden_records': 200, 'public_replay': 'pending'}))
