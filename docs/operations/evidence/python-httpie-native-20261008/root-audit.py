import hashlib
import json
import pathlib
import subprocess

base = pathlib.Path('/private/tmp/sol61-httpie-python-report-20261008')
repo = pathlib.Path('/Users/mileswirht/Downloads/nudox-active-20261005/sol61-python-project-report-mismatch-20261008')
sha = lambda data: hashlib.sha256(data).hexdigest()
source = 'b97584982d77f51b1bb182426f4be77b2396b481'
for phase in ['candidate-build-01', 'candidate-test-01', 'candidate-run-01']:
    directory = base / 'raw-proof' / phase
    receipt = json.loads((directory / 'receipt.json').read_text())
    assert receipt['exit_code'] == 0
    assert receipt['source_before'] == receipt['source_after']
    assert receipt['source_before']['commit'] == source
    assert receipt['source_before']['clean']
    assert receipt['retirement'] == 'owned-child-kernel-wait'
    assert receipt['managed_leases_empty'] and receipt['original_stamps_restored']
    for stream in ['stdout', 'stderr']:
        if stream + '_sha256' in receipt:
            assert sha((directory / (stream + '.log')).read_bytes()) == receipt[stream + '_sha256']

directory = base / 'raw-proof' / 'candidate-test-01'
stdout = (directory / 'stdout.log').read_text()
assert 'test result: ok. 9 passed; 0 failed; 0 ignored;' in stdout
assert 'test native_empty_package_initializers_are_not_named_declaration_targets ... ok' in stdout
fresh = [json.loads(line) for line in stdout.splitlines() if line.startswith('{')]
fresh = [row for row in fresh if row.get('reason') == 'compiler-artifact' and not row.get('fresh')]
assert any(row['target']['name'] == 'backend_frontend_python' for row in fresh)
assert any(row['target']['name'] == 'python_project' for row in fresh)

inputs = json.loads((directory / 'source-inputs.json').read_text())
source_inputs_checked = 0
for relative, expected in inputs.items():
    data = (repo / relative).read_bytes()
    if sha(data) != expected:
        data = subprocess.check_output(['git', 'show', source + ':' + relative], cwd=repo)
    assert sha(data) == expected, relative
    source_inputs_checked += 1
assert source_inputs_checked == 2067

proof = json.loads((base / 'httpie-causal-native-proof.json').read_text())
for relative, expected in proof['source_sha256'].items():
    assert sha((base / 'project' / relative).read_bytes()) == expected, relative
assert len(proof['source_sha256']) == 265
script = (base / 'audit-native-report.py').read_text()
script = script.replace("root=pathlib.Path('/Users/rmccrar6/sol61-python-report-20261008');project=root/'httpie-project';report=root/'candidate-run-01/stdout.log'", "root=pathlib.Path('/private/tmp/sol61-httpie-python-report-20261008');project=root/'project';report=root/'candidate-run-01.stdout.json'")
script = script.replace("p=root/'httpie-causal-native-proof.json'", "p=root/'root-independent-native-proof.json'")
exec(compile(script, 'reviewed-coordinate-audit', 'exec'), {})
independent = json.loads((base / 'root-independent-native-proof.json').read_text())
assert independent == proof
result = {'source': source, 'source_inputs_checked': source_inputs_checked, 'native_tests': {'passed': 9, 'failed': 0, 'ignored': 0}, 'real_source_files': 265, 'python_modules_per_fresh_state': 133, 'fresh_states': 2, 'native_coordinates_checked': independent['source_and_program_coordinates_checked'], 'root_coordinate_replay_matches': True, 'scope': 'native compiler authority only; installed CLI/MCP acceptance remains open'}
(base / 'root-independent-audit.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
