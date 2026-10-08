import datetime
import hashlib
import json
import pathlib
import re
import subprocess

ROOT = pathlib.Path('/Users/mileswirht/Downloads/sol61-journal-public-recovery-20261008/native11-green-raw-evidence')
REPO = '/private/tmp/nudox-journal-publication-checkpoint-20261008'
BASE = '1b46051a4bbc69440092bc7be41d80c918419282'
SOURCE = 'ea934be4f0f1d5d6f1e96d61a4e5bbd19fea4f8d'
CANONICAL = '49bc00a8e77cc375d15426665bee92f204ea6d1e'
INTEGRATED = '364849fe0d'

def git(*args):
    return subprocess.check_output(['git', *args], cwd=REPO)

def sha(data):
    return hashlib.sha256(data).hexdigest()

seal = json.loads((ROOT / 'audit-seal.json').read_text())
for name, expected in seal['files'].items():
    data = (ROOT / name).read_bytes()
    assert len(data) == expected['bytes'] and sha(data) == expected['sha256'], name
inputs = json.loads((ROOT / 'source-inputs.json').read_text())
assert inputs['base'] == BASE and inputs['source']['commit'] == SOURCE
assert git('rev-parse', SOURCE + '^{tree}').decode().strip() == inputs['source']['tree']
assert sha(git('show', SOURCE + ':Cargo.lock')) == inputs['source']['lock_sha256']
changed = git('diff', '--name-only', BASE, SOURCE).decode().splitlines()
assert set(changed) == {row['path'] for row in inputs['files']}
for row in inputs['files']:
    assert sha(git('show', BASE + ':' + row['path'])) == row['before_sha256']
    assert sha(git('show', SOURCE + ':' + row['path'])) == row['after_sha256']
    assert git('rev-parse', SOURCE + ':' + row['path']).decode().strip() == row['after_blob']
assert git('diff', BASE, SOURCE) == (ROOT / 'graph-fifo-complete.diff').read_bytes()

# Two source files are exact. The adapter preserves canonical's existing typed
# read repair and adds the exact reviewed native11 test block at the same seam.
for path in changed[:]:
    if path.endswith('/adapter.rs'):
        canonical = git('show', CANONICAL + ':' + path)
        source = git('show', SOURCE + ':' + path)
        anchor = b'    #[derive(Clone)]\n    struct MixedNativeEnvironment'
        start = source.index(b'    #[test]\n    fn official_httpie_dependency_graph_keeps_package_and_search_surfaces_readable()')
        end = source.index(anchor, start)
        assert canonical.count(anchor) == 1
        expected = canonical.replace(anchor, source[start:end] + anchor)
        assert git('show', INTEGRATED + ':' + path) == expected
    else:
        assert git('show', INTEGRATED + ':' + path) == git('show', SOURCE + ':' + path)
assert git('show', INTEGRATED + ':Cargo.lock') == git('show', CANONICAL + ':Cargo.lock')

receipt = json.loads((ROOT / 'receipt.json').read_text())
assert receipt['source_before'] == receipt['source_after'] == inputs['source']
assert receipt['exit'] == 0 and receipt['retirement'] == 'owned-child-kernel-wait'
assert receipt['admission']['advisory_allowed']
admission = receipt['admission']
assert admission['current_fleet_compiler_group_count'] < admission['fleet_compiler_group_limit'] == 16
host = admission['hosts'][admission['destination']]
assert host['compiler_group_count'] < host['host_limit']
assert host['available_memory_bytes'] >= 8 * 1024**3
assert host['root_disk_available_bytes'] >= 16 * 1024**3
assert all(h['complete'] and isinstance(h['available_memory_bytes'], int) for h in admission['hosts'].values())
age = datetime.datetime.fromisoformat(receipt['started_utc']) - datetime.datetime.fromisoformat(admission['collector_started_at_utc'])
assert 0 <= age.total_seconds() <= 30
assert '-j2' in receipt['argv'] and '--include-ignored' in receipt['argv']
log = (ROOT / 'cargo.log').read_text()
results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', log)
assert results == [('19', '0', '0'), ('2', '0', '0')]
names = re.findall(r'^test ([^\s]+) \.\.\. ', log, re.M)
assert len(names) == len(set(names)) == 21
assert len(re.findall(r'^test .+ \.\.\. ok$', log, re.M)) + len(re.findall(r'^ok$', log, re.M)) == 21
assert 'files=265 bytes=2096089 python_files=133' in log
assert not re.search(r'^error\[E\d+\]', log, re.M)
assert seal['images'] == receipt['test_binaries']
assert seal['test_results'] == receipt['test_results']
prior = json.loads(pathlib.Path('/private/tmp/nudox-root-journal-native09-audit-20261008.json').read_text())
assert prior['source'] == BASE and prior['tracked_source_entries_verified'] == 17647
result = {
    'source': SOURCE, 'tree': inputs['source']['tree'], 'base': BASE,
    'base_tracked_source_entries_previously_verified': prior['tracked_source_entries_verified'],
    'changed_paths_verified': changed, 'integrated_source': git('rev-parse', INTEGRATED).decode().strip(),
    'canonical_preserved': CANONICAL, 'passed': 21, 'failed': 0, 'ignored': 0,
    'test_seconds': [0.02, 4.70], 'total_seconds': receipt['elapsed_seconds'],
    'source_archive_scope': 'Previously audited exact native09 archive plus all three native11 Git blob deltas.',
    'image_seals': seal['images'], 'seal_sha256': sha((ROOT / 'audit-seal.json').read_bytes()),
    'receipt_sha256': sha((ROOT / 'receipt.json').read_bytes()), 'log_sha256': sha((ROOT / 'cargo.log').read_bytes()),
    'limits': [
        'Warm/cold six-route dependency graph controls use the actual service adapter and strict DTO admission, not installed CLI/MCP transport.',
        'Full pinned HTTPie source is copied; the graph-only control correctly returns zero native search declarations.',
        'Remote native image hashes are supervisor seals, not independent Root remote image rereads.',
        'Combined canonical integration retains the separately tested typed-read repair; combined release/public replay is still pending.',
    ],
}
out = pathlib.Path('/private/tmp/nudox-root-python-graph-native11-audit-20261008.json')
out.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
