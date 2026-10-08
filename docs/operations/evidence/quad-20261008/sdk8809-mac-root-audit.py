import hashlib
import json
import re
import subprocess
import tarfile
from pathlib import Path, PurePosixPath

archive = Path('/private/tmp/8809-mac-native-source-bound-proof-20261008.tar.gz')
checkout = Path('/Users/mileswirht/Downloads/nudox-active-20261005/root-quality-integration-20261007')
out = Path('/private/tmp/nudox-root-sdk8809-mac-audit.json')
sha = lambda b: hashlib.sha256(b).hexdigest()
with tarfile.open(archive) as tar:
    members = tar.getmembers()
    assert len(members) == 24
    assert len({m.name for m in members}) == len(members)
    assert all(m.isfile() and not PurePosixPath(m.name).is_absolute() and '..' not in PurePosixPath(m.name).parts for m in members)
    raw = {m.name: tar.extractfile(m).read() for m in members}
base = 'Users/rmccrar6/nudox-functional-corpus-20261006/sol61-current-sdk-8809-native-20261008/'
load = lambda path: json.loads(raw[path])
plan_name = 'private/tmp/selected-sdk-8809-mac-native-plan-20261008.json'
plan = load(plan_name)
supervisor_name = 'private/tmp/selected-sdk-e42-native-supervisor.py'
expected = {
    'commit': '8809eb34a5c44ee83145c143b9d444124542adc8',
    'tree': 'aa8ddaad4213ac5e84368fa96cdc59a1963221c0',
    'clean': True,
    'cargo_lock_sha256': '5d0b0b75a5acb9fd95db98d160ede1e3c780069cdc9989561491fe2cb5ba3da0',
}
assert subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=checkout, text=True).strip() == expected['commit']
assert subprocess.check_output(['git', 'rev-parse', 'HEAD^{tree}'], cwd=checkout, text=True).strip() == expected['tree']
assert not subprocess.check_output(['git', 'status', '--porcelain'], cwd=checkout)
assert sha((checkout / 'Cargo.lock').read_bytes()) == expected['cargo_lock_sha256']
assert sha(raw[plan_name]) == '5c5165001f77ea0cc2854c7b9e3d825aec4be8dfbf7e10af6b0d4fe3bdee4a8f'
build_raw = raw[base + 'build-01/receipt.json']
build = json.loads(build_raw)
assert build['compiler_source_bound'] is True and build['rustc_processes']
assert any(p['cwd'] == build['cwd'] and 'crates/engine/src/lib.rs' in p['args'] for p in build['rustc_processes'])
events = []
for line in raw[base + 'build-01/stdout.log'].splitlines():
    try:
        event = json.loads(line)
    except ValueError:
        continue
    events.append(event)
artifact = build['artifact']
assert any(e.get('reason') == 'compiler-artifact' and e.get('executable') == artifact['path'] and e['fresh'] is False for e in events)
assert any(e.get('reason') == 'build-finished' and e['success'] is True for e in events)
results = {}
for gate, counts in [('build-01', None), ('host-01', (39, 0, 1)), ('typescript-01', (31, 0, 0)), ('classifier-01', (2, 0, 0))]:
    prefix = base + gate + '/'
    receipt = load(prefix + 'receipt.json')
    assert receipt['source_before'] == receipt['source_after'] == expected
    assert receipt['frozen_graph_before'] == receipt['frozen_graph_after']
    assert receipt['exit'] == 0 and receipt['retirement'] == 'owned-child-kernel-wait' and receipt['managed_leases_empty'] is True
    assert receipt['plan_sha256'] == sha(raw[plan_name])
    assert sha(raw[prefix + 'stdout.log']) == receipt['stdout_sha256']
    assert sha(raw[prefix + 'stderr.log']) == receipt['stderr_sha256']
    inputs = load(prefix + 'source-inputs.json')
    assert len(inputs) == receipt['source_input_count'] == 2386
    assert sha(json.dumps(inputs, sort_keys=True, separators=(',', ':')).encode()) == receipt['source_input_manifest_sha256']
    for path, info in inputs.items():
        assert info['kind'] == 'file'
        assert sha((checkout / path).read_bytes()) == info['sha256'], path
    launch = load(prefix + 'launch.json')
    assert launch['source_before'] == expected and launch['argv'] == receipt['argv']
    if counts is not None:
        assert receipt['build_receipt_sha256'] == sha(build_raw)
        assert receipt['artifact_before'] == receipt['artifact_after'] == artifact
        assert receipt['argv'][0] == artifact['path']
        match = re.search(rb'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', raw[prefix + 'stdout.log'])
        assert match and tuple(map(int, match.groups())) == counts
        results[gate] = dict(zip(['passed', 'failed', 'ignored'], counts))
manifest = load(base + 'native-fixture-manifest.json')
assert manifest['source'] == expected and manifest['compiled_artifact'] == artifact
assert manifest['immutable_image']['sha256'] == artifact['sha256']
assert manifest['immutable_image']['bytes'] == artifact['bytes']
for gate in results:
    assert manifest['fixture_results'][gate].encode() == raw[base + gate + '/stdout.log']
returned_name = 'Users/rmccrar6/nudox-functional-corpus-20261006/sol61-nonblocking-search/sdk-8809-exclusive-graph-return-20261008.json'
returned = load(returned_name)
assert returned['source'] == expected and returned['owned_children_absent'] and returned['managed_leases_empty']
for gate, digest in returned['receipts'].items():
    assert digest == sha(raw[base + gate + '/receipt.json'])
report = {
    'source': expected,
    'archive_sha256': sha(archive.read_bytes()),
    'archive_members': len(raw),
    'all_receipt_logs_and_source_input_digests_verified': True,
    'source_inputs_verified_against_root_checkout_per_gate': 2386,
    'actual_fresh_engine_artifact': artifact,
    'native_results': results,
    'total_passed': sum(v['passed'] for v in results.values()),
    'total_ignored': sum(v['ignored'] for v in results.values()),
    'retirement_and_graph_return_verified': True,
    'immutable_image_independent_remote_hash': 'PENDING',
    'installed_real_project_acceptance': 'PENDING',
    'status': 'PASS for exact-source native controls; no installed-app acceptance claim',
}
out.write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
