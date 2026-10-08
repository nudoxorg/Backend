import datetime
import hashlib
import json
import pathlib
import re
import subprocess
import tarfile

ROOT = pathlib.Path('/Users/mileswirht/Downloads/sol61-journal-public-recovery-20261008/journal-native09-integration-audit')
REPO = '/private/tmp/nudox-cli-mcp-reliability-checkpoint-20261008'
HEAD = '1b46051a4bbc69440092bc7be41d80c918419282'

def git(*args):
    return subprocess.check_output(['git', *args], cwd=REPO)

def sha(data):
    return hashlib.sha256(data).hexdigest()

seal = json.loads((ROOT / 'audit-seal.json').read_text())
for name, expected in seal.items():
    data = (ROOT / name).read_bytes()
    assert len(data) == expected['bytes'] and sha(data) == expected['sha256'], name

inputs = json.loads((ROOT / 'source-inputs.json').read_text())
for entry in inputs['files']:
    for side in ['before', 'after']:
        expected = entry[side]
        if expected is None:
            continue
        commit = inputs['base' if side == 'before' else 'head']['commit']
        data = git('show', commit + ':' + entry['path'])
        assert sha(data) == expected['sha256'] and len(data) == expected['bytes']
        assert git('rev-parse', commit + ':' + entry['path']).decode().strip() == expected['blob']

inventory = {}
for entry in git('ls-tree', '-rz', HEAD).split(b'\0'):
    if not entry:
        continue
    metadata, name = entry.split(b'\t', 1)
    mode, kind, blob = metadata.split()
    if kind == b'blob':
        inventory[name.decode()] = (mode.decode(), blob.decode())
actual = {}
with tarfile.open(ROOT / 'native09-source.tar.gz') as tar:
    for member in tar:
        name = member.name.removeprefix('./')
        assert not name.startswith('/') and '..' not in pathlib.PurePosixPath(name).parts
        if member.isdir():
            continue
        if member.issym():
            data = member.linkname.encode()
            mode = '120000'
        else:
            assert member.isfile(), name
            data = tar.extractfile(member).read()
            mode = '100755' if member.mode & 0o111 else '100644'
        blob = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        actual[name] = (mode, blob)
assert actual == inventory, (set(actual) ^ set(inventory), [p for p in actual.keys() & inventory.keys() if actual[p] != inventory[p]][:10])

receipt = json.loads((ROOT / 'native09-receipt.json').read_text())
assert receipt['source_before'] == receipt['source_after']
assert receipt['source_before']['commit'] == HEAD and receipt['source_before']['clean']
assert receipt['exit'] == 0 and receipt['retirement'] == 'owned-child-kernel-wait'
assert sha(git('show', HEAD + ':Cargo.lock')) == receipt['source_before']['lock_sha256']
assert receipt['admission']['advisory_allowed']
age = datetime.datetime.fromisoformat(receipt['started_utc']) - datetime.datetime.fromisoformat(receipt['admission']['collector_started_at_utc'])
assert 0 <= age.total_seconds() <= 30
log = (ROOT / 'native09-cargo.log').read_text()
results = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;[^\n]*', log)
assert len(results) == 1 and results[0] == ('50', '0', '0')
names = re.findall(r'^test ([^\s]+) \.\.\. ', log, re.M)
assert len(names) == len(set(names)) == 50
# --nocapture inserts producer diagnostics between the test prefix and `ok`.
assert len(re.findall(r'^test .+ \.\.\. ok$', log, re.M)) + len(re.findall(r'^ok$', log, re.M)) == 50
assert not re.search(r'^error\[E\d+\]', log, re.M)
image = json.loads((ROOT / 'native09-image-seal.json').read_text())
assert image['receipt_sha256'] == sha((ROOT / 'native09-receipt.json').read_bytes())
assert image['log_sha256'] == sha((ROOT / 'native09-cargo.log').read_bytes())
assert image['source'] == receipt['source_before']
assert image['image']['sha256'] == receipt['test_binaries'][0]['sha256']
result = {'source': HEAD, 'tree': inputs['head']['tree'], 'tracked_source_entries_verified': len(actual),
          'changed_source_paths_verified': len(inputs['files']), 'passed': 50, 'failed': 0, 'ignored': 0,
          'test_seconds': 47.61, 'total_seconds': receipt['elapsed_seconds'],
          'native_test_image': image['image'], 'source_archive_sha256': seal['native09-source.tar.gz']['sha256'],
          'receipt_sha256': image['receipt_sha256'], 'log_sha256': image['log_sha256'],
          'limits': ['Focused source-bound native journal controls; not installed public product acceptance.',
                     'Remote native image hash is sealed by the supervisor, not independently reread by Root here.',
                     'Later Python dependency-graph and FIFO correction gates are separate and not credited.']}
out = pathlib.Path('/private/tmp/nudox-root-journal-native09-audit-20261008.json')
out.write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
