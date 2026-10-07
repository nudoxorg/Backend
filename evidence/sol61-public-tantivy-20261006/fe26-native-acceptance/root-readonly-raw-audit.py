import hashlib
import json
import pathlib
import sys
import time
from datetime import datetime

spec = json.loads(pathlib.Path(sys.argv[1]).read_text())
started = time.time()
checked = []


def digest(path):
    h = hashlib.sha256()
    count = 0
    with path.open('rb') as f:
        while True:
            block = f.read(1024 * 1024)
            if not block:
                break
            count += len(block)
            h.update(block)
    return count, h.hexdigest()


for artifact in spec['artifacts']:
    path = pathlib.Path(artifact['path'])
    assert path.is_absolute()
    size, sha = digest(path)
    assert (size, sha) == (artifact['bytes'], artifact['sha256']), str(path)
    checked.append({'path': str(path), 'bytes': size, 'sha256': sha})

package_root = pathlib.Path('/Users/rmccrar6/nudox-functional-corpus-20261006/sol-fresh-remote-runtime/artifacts/cli-canonical-fe26-macos-arm64-v1/nudox-macos-arm64')
binaries = []
for artifact in spec['relocated_binary_manifest']['files']:
    original = pathlib.Path(artifact['original'])
    _, original_sha = digest(original)
    assert original_sha == artifact['original_sha256'], str(original)
    packaged = package_root / artifact['packaged']
    size, packaged_sha = digest(packaged)
    assert packaged_sha == artifact['packaged_sha256'], str(packaged)
    binaries.append({'path': str(packaged), 'bytes': size, 'sha256': packaged_sha})

launches = []
for phase in spec['phase_checks']:
    wire_path = next(a['path'] for a in spec['artifacts']
                     if a['path'].endswith('/' + phase['phase'] + '/wire.jsonl'))
    with pathlib.Path(wire_path).open() as wire:
        for line in wire:
            row = json.loads(line)
            if row['kind'] == 'owner-start':
                break
        else:
            raise AssertionError('no actual owner-start: ' + phase['phase'])
    assert row['pid'] == phase['owner_pid']
    sampled = datetime.fromisoformat(phase['fleet_sampled_at_utc']).timestamp()
    sample_delay = row['at_unix'] - sampled
    oldest = max(phase['host_sample_ages'].values()) + sample_delay
    assert 0 <= sample_delay <= 30 and oldest <= 30, (phase['phase'], oldest)
    launches.append({'phase': phase['phase'], 'actual_owner_pid': row['pid'],
                     'sample_to_owner_start_seconds': sample_delay,
                     'oldest_host_sample_seconds_at_owner_start': oldest})

result = {
    'schema': 'nudox.root-readonly-raw-audit.v1',
    'source': spec['tested_source'],
    'evidence_commit': spec['evidence_commit'],
    'verified_raw_artifacts': len(checked),
    'verified_packaged_files': len(binaries),
    'derived_committed_totals': spec['totals'],
    'scope': spec['scope'],
    'artifacts': checked,
    'packaged_files': binaries,
    'actual_launch_freshness': launches,
    'duration_seconds': time.time() - started,
    'no_runtime_launched_or_state_modified': True,
}
pathlib.Path(sys.argv[2]).write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps({k: v for k, v in result.items() if k not in ('artifacts', 'packaged_files')}))
