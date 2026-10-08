import gzip
import hashlib
import json
import subprocess
from pathlib import Path

root = Path('/Users/mileswirht/Downloads/backend-sol61-search-joined-tests')
prefix = root / 'evidence/sol61-current-search-docs-5f2-20261008'
def digest(data):
    return hashlib.sha256(data).hexdigest()
def git(*args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
def member(name):
    path = prefix / name
    assert path.resolve().is_relative_to(prefix.resolve()) and not path.is_symlink()
    return path.read_bytes()

index = json.loads(member('member-index.json'))
inventory = json.loads(member('inventory.json'))
assert len(index) == 140 and len(inventory) == 139
for name, proof in index.items():
    data = member(name)
    assert len(data) == proof['bytes'] and digest(data) == proof['sha256'], name
for name, proof in inventory.items():
    stored = member(name)
    assert len(stored) == proof['stored_bytes'] and digest(stored) == proof['stored_sha256'], name
    raw = gzip.decompress(stored) if proof['compressed'] else stored
    assert len(raw) == proof['raw_bytes'] and digest(raw) == proof['raw_sha256'], name

proof = json.loads(member('native-mac-07/compact-proof.json'))
manifest_data = member('native-mac-07/manifest.json')
manifest = json.loads(manifest_data)
receipt = json.loads(gzip.decompress(member('native-mac-07/receipt.json.gz')))
assert digest(manifest_data) == proof['manifest_sha256']
assert proof['source_before'] == proof['source_after'] == receipt['source_before'] == receipt['source_after']
assert proof['source_before']['clean'] and proof['exit'] == receipt['exit'] == 0
assert proof['retirement'] == receipt['retirement'] == 'owned-child-kernel-wait'
assert proof['source_before']['commit'] == manifest['source_commit'] == '5f2a97eb4c74c175e190ff9637373c6bcf9ae39e'
for name, artifact in manifest['artifacts'].items():
    assert {k: artifact[k] for k in ['sha256', 'bytes']} == proof['artifacts'][name]
    assert artifact['exact_cargo_event'] == receipt['artifact_events'][name]
    assert artifact['exact_cargo_event']['target']['name'] == name
assert set(manifest['artifacts']) == {'backend-cli', 'backend-mcp', 'backend-locald'}
for name, raw in proof['raw'].items():
    retained = member('native-mac-07/' + name + '.gz')
    assert digest(gzip.decompress(retained)) == raw['sha256'], name

analysis = json.loads(member('docs03/analysis.json'))
controls = json.loads(gzip.decompress(member('docs03/control-responsiveness.json.gz')))
assert analysis['manifest_sha256'] == digest(manifest_data)
assert analysis['fresh_empty_home_acceptance'] is False
assert analysis['all_mcp_transport_surface_calls_success'] is True
assert analysis['mcp_semantic_correctness_all_pass'] is False
assert analysis['references_semantics']['record_count'] == 0
assert analysis['graph_semantics']['semantic_edge_count'] is None
assert analysis['controls_count'] == 37 and analysis['pending_controls_count'] == 36
canonical = git('rev-parse', 'origin/canonical')
changed = git('diff', '--name-only', manifest['source_commit'], canonical).splitlines()
non_product = ['docs/', 'evidence/', '.config/scripts/']
product_changes = [p for p in changed if not any(p.startswith(x) for x in non_product)]
assert not product_changes, product_changes
result = {
    'reviewer': 'root',
    'source_evidence_commit': '403a891d50dfc8bc6622a78ceaf27c692822e014',
    'native_source': manifest['source_commit'],
    'canonical_compared': canonical,
    'production_changed_paths': product_changes,
    'excluded_non_product_prefixes': non_product,
    'indexed_stored_members_verified': len(index),
    'decompressed_raw_members_verified': len(inventory),
    'native_manifest_sha256': digest(manifest_data),
    'native_source_receipt_artifact_binding_verified': True,
    'executable_bytes_rehashed_by_root_in_this_audit': False,
    'executable_hash_scope': 'Compared retained Cargo events, build receipt, manifest and runtime identity. Actual executable copying/hash verification was the remote native supervisor; this audit does not claim another executable rehash.',
    'actual_runtime_controls': {'total': 37, 'while_search_pending': 36, 'maximum_seconds': analysis['max_control_seconds']},
    'mcp_transport_surface_calls': analysis['mcp_calls_count'],
    'references_functional_result': 'FAIL: 0 records with a pinned authentic caller',
    'graph_semantics': 'Not established: symbol records are not edge evidence',
    'fresh_empty_home_acceptance': False,
    'native_tests_on_current_source': 0,
    'predecessor_tests_relabelled': False,
    'audit_script_sha256': digest(Path(__file__).read_bytes()),
}
destination = Path('/private/tmp/nudox-root-current-docs-audit-20261008.json')
with destination.open('x') as output:
    json.dump(result, output, indent=2)
    output.write('\n')
print(json.dumps(result))
