#!/usr/bin/env python3
"""Audit retained focused native evidence and the exact integration file map."""
import argparse
import hashlib
import json
import subprocess
from pathlib import Path

p = argparse.ArgumentParser()
p.add_argument('--evidence', type=Path, required=True)
p.add_argument('--filemap', type=Path, required=True)
p.add_argument('--repo', type=Path, required=True)
p.add_argument('--integration-ref', required=True)
p.add_argument('--output', type=Path, required=True)
a = p.parse_args()
m = json.loads((a.evidence / 'evidence-manifest.json').read_text())
fm = json.loads(a.filemap.read_text())
checks = []

def git(*args):
    return subprocess.check_output(['git', *args], cwd=a.repo)

def checked(binding):
    original = Path(binding['path'])
    local = a.evidence / original.relative_to('/private/tmp/luna-current-virtual-manifest-7382-evidence')
    raw = local.read_bytes()
    assert hashlib.sha256(raw).hexdigest() == binding['sha256'], local
    checks.append(str(local.relative_to(a.evidence)))
    return raw

for key in ['plan', 'loan_receipt', 'return_and_next_loan_receipt']:
    checked(m[key])
assert m['source']['commit'] == fm['commit']['sha1']
assert m['source']['tree'] == fm['commit']['tree_sha1']
assert m['source']['cargo_lock_sha256'] == fm['commit']['cargo_lock_sha256']
assert hashlib.sha256(git('diff', '--abbrev=8', a.integration_ref + '^', a.integration_ref)).hexdigest() == fm['reviewed_diff_sha256']
assert not m['supervisor_binary_mapping']['complete']
for g in m['gates']:
    cw = json.loads(checked(g['crosswalk']))
    raw = {name: checked(binding) for name, binding in g['raw'].items()}
    checked(g['v5_admission'])
    fleet = json.loads(raw['fleet.json'])
    assert fleet['advisory_allowed'] is True
    completion = json.loads(raw['completion.json'])
    assert completion['exit'] == 0
    assert completion['retirement'] == 'owned-child-kernel-wait'
    for field in ['source_unchanged', 'frozen_graph_unchanged', 'managed_permit_retired', 'managed_graph_leases_empty']:
        assert completion[field] is True, field
    assert completion['executed_binary_results'] == []
    assert completion['binary_result_mapping_complete'] is False
    assert completion['test_results'] == [g['test_result']]
    assert cw['test']['result'] == g['test_result']
    assert cw['test']['name'] == g['test']
    assert cw['binary'] == m['binary']
    for field in ['commit', 'tree', 'worktree', 'cargo_lock_sha256']:
        assert cw['source'][field] == m['source'][field]
    assert cw['test']['running_line'] in raw['cargo.stderr.log'].decode()
    assert cw['binary']['path'] in cw['test']['running_line']
    stdout = raw['cargo.stdout.jsonl'].decode()
    assert g['test'] + ' ... ok' in stdout
    assert g['test_result'] in stdout
    assert '1 passed; 0 failed; 0 ignored;' in g['test_result']

changed = git('diff', '--name-only', a.integration_ref + '^', a.integration_ref).decode().splitlines()
assert sorted(changed) == sorted(row['path'] for row in fm['changed_files'])
for row in fm['changed_files']:
    path = row['path']
    for ref, prefix in [(a.integration_ref + '^', 'before'), (a.integration_ref, 'after')]:
        blob = git('rev-parse', f'{ref}:{path}').decode().strip()
        assert blob == row[f'git_blob_{prefix}_sha1']
        assert hashlib.sha256(git('show', f'{ref}:{path}')).hexdigest() == row[f'file_{prefix}_sha256']
assert hashlib.sha256(git('show', a.integration_ref + ':Cargo.lock')).hexdigest() == m['source']['cargo_lock_sha256']
report = {
    'schema': 'nudox.root.virtual-cargo-manifest.audit.v1',
    'status': 'PASS',
    'native_source': m['source'],
    'integration_commit': git('rev-parse', a.integration_ref).decode().strip(),
    'integration_base': git('rev-parse', a.integration_ref + '^').decode().strip(),
    'native_tests_passed': 4,
    'verified_raw_file_bindings': len(checks),
    'verified_unique_raw_files': len(set(checks)),
    'source_files_exact': changed,
    'cargo_lock_unchanged': True,
    'reviewed_diff_git_abbrev': 8,
    'supervisor_binary_mapping_complete': False,
    'supplement': 'Exact verbose Cargo Running lines, matching result text and remote executable identity in four crosswalks.',
    'limits': ['No local rehash of the 723 MB remote executable.', 'No full library suite, native real-project compiler run, Clippy or whole-app proof.', 'Constructed public-search control is not a native Rust compiler acceptance test.'],
}
a.output.write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))
