#!/usr/bin/env python3
"""Private development-mode CLI/owner oracle for mixed Python/SQL publication.

Failures retain the private state and record process-retirement evidence. A pass
requires the Root build receipt and actual binary identities to remain unchanged.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

from sql_journey_safety import OwnedProcesses, digest, remove_private_root, verified_build

p = argparse.ArgumentParser()
p.add_argument('--bundle', required=True, type=Path)
p.add_argument('--output', required=True, type=Path)
p.add_argument('--build-receipt', required=True, type=Path)
p.add_argument('--source-checkout', required=True, type=Path)
p.add_argument('--without-worker', action='store_true')
p.add_argument('--wait-seconds', type=int, default=120)
p.add_argument('--startup-seconds', type=int, default=600)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=False)
a.output = a.output.resolve()
private = Path(tempfile.mkdtemp(prefix='min-', dir=a.output.parent))
os.chmod(private, 0o700)
private_stat = private.lstat()
private_identity = (private_stat.st_dev, private_stat.st_ino)
processes = None
receipt = {'result': 'failed', 'development_mode': True, 'private_root': str(private),
           'state': str(private / 'state'), 'project': str(private / 'project'),
           'commands': [], 'cleanup': {'removed': False}}
owner = None
initial = None
identities = {}
try:
    processes = OwnedProcesses()
    source_bundle = a.bundle.resolve(strict=True)
    initial = verified_build(a.build_receipt, a.source_checkout, source_bundle, __file__)
    receipt['build_before'] = initial
    bundle = private / 'bundle'
    bundle.mkdir()
    for name in ('backend-cli', 'backend-locald', 'backend-mcp', 'backend-sqlite-worker'):
        if name == 'backend-sqlite-worker' and a.without_worker:
            continue
        source = (source_bundle / name).resolve()
        if not source.is_file():
            if name == 'backend-sqlite-worker':
                continue
            raise RuntimeError(f'missing {name}')
        identities[name] = digest(source)
        # A private copy avoids changing the provenance artifact's link count.
        shutil.copy2(source, bundle / name)
        if digest(bundle / name) != identities[name]:
            raise RuntimeError(f'private copy of {name} changed')
    receipt['binaries'] = identities
    project = private / 'project'
    project.mkdir()
    (project / 'repository.py').write_text('def load_query():\n    return 1\n')
    (project / 'schema.sql').write_text('CREATE TABLE books (\n id INTEGER PRIMARY KEY,\n title TEXT\n);\n')
    state = private / 'state'
    state.mkdir(mode=0o700)
    secret = state / 'authority.secret'
    secret.write_bytes(os.urandom(32))
    secret.chmod(0o600)
    endpoint = private / 'owner.sock'
    env = {k: os.environ[k] for k in ('PATH', 'LANG', 'TZ') if k in os.environ}
    for key in ('HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'XDG_STATE_HOME', 'XDG_RUNTIME_DIR', 'TMPDIR'):
        env[key] = str(private / key)
        Path(env[key]).mkdir(mode=0o700)
    env['BACKEND_LOCALD_AUTHORITY_SECRET_FILE'] = str(secret)
    env['NUDOX_TRACE'] = '1'

    def cli(*args):
        command = [str(bundle / 'backend-cli'), '--format', 'json', '--project', str(project), '--workspace', str(state), '--endpoint', str(endpoint), *args]
        start = time.monotonic()
        result = processes.run(command, cwd=project, env=env, timeout=a.wait_seconds + 120)
        record = {'argv': command, 'exit': result.returncode, 'seconds': time.monotonic() - start, 'stdout': result.stdout.decode(errors='replace'), 'stderr': result.stderr.decode(errors='replace')}
        receipt['commands'].append(record)
        return record

    for turn in range(2):
        if endpoint.exists():
            endpoint.unlink()
        with (a.output / f'owner-{turn}.stderr').open('wb') as stderr:
            owner = processes.spawn([str(bundle / 'backend-locald'), '--endpoint', str(endpoint), '--workspace', str(state), '--authority-secret-file', str(secret), '--registry-offline', '--registry-discovery-offline', '--advisory-offline', '--forge-offline'], cwd=project, env=env, stdout=subprocess.DEVNULL, stderr=stderr)
        deadline = time.monotonic() + a.startup_seconds
        while not endpoint.exists():
            if processes.exited(owner) or time.monotonic() > deadline:
                raise RuntimeError('owner failed to start')
            time.sleep(0.05)
        if turn == 0:
            index = cli('add', str(project), '--wait', '--wait-timeout', str(a.wait_seconds))
            receipt['lease_directories'] = {str(path.relative_to(state)): len(list(path.iterdir())) for path in state.rglob('*') if path.is_dir() and 'lease' in path.name}
            if index['exit'] != 0:
                cli('status')
                raise RuntimeError(f"mixed publication failed: exit {index['exit']}")
        status = cli('status')
        if status['exit'] != 0:
            raise RuntimeError('selected SQL status failed')
        summary = json.loads(status['stdout']).get('sql_analysis')
        expected_count = 'no_worker_files' if a.without_worker else 'analysed_files'
        if not isinstance(summary, dict) or summary.get(expected_count) != 1:
            raise RuntimeError(f'status did not preserve selected SQL {expected_count}: {summary}')
        gap = cli('sql-query', str(project), '--role', 'gap')
        if gap['exit'] != 0:
            raise RuntimeError('SQL gap query failed')
        if a.without_worker and ('schema.sql' not in gap['stdout'] or 'worker' not in gap['stdout'].lower()):
            raise RuntimeError('missing worker gap did not name the SQL file and reason')
        processes.retire(owner, crash=True)
        owner = None
    receipt['result'] = 'passed'
except Exception as error:
    receipt['error'] = str(error)
finally:
    failures = processes.retire_all() if processes is not None else [{'kind': 'descendant-proof', 'error': 'process custody was not established'}]
    receipt['cleanup'] = {'retired': processes.proofs if processes is not None else [], 'unretired': failures, 'removed': False}
    if failures:
        receipt['result'] = 'failed'
        receipt.setdefault('error', 'owned process retirement could not be proven')
        receipt['cleanup_error'] = 'owned process retirement could not be proven'
    if initial is not None:
        try:
            after = verified_build(a.build_receipt, a.source_checkout, source_bundle, __file__)
            if after != initial:
                raise RuntimeError('Root receipt, source, binaries, or harness changed during journey')
            for name, expected in identities.items():
                if digest(source_bundle / name) != expected or digest(bundle / name) != expected:
                    raise RuntimeError(f'{name} changed during journey')
            receipt['build_after'] = after
        except Exception as error:
            receipt['result'] = 'failed'
            receipt['provenance_error'] = str(error)
    if receipt['result'] == 'passed':
        try:
            remove_private_root(private, private_identity)
            receipt['cleanup']['removed'] = True
        except Exception as error:
            receipt['result'] = 'failed'
            receipt['error'] = f'private root cleanup failed: {error}'
    (a.output / 'receipt.json').write_text(json.dumps(receipt, indent=2))
print(json.dumps({'result': receipt['result'], 'output': str(a.output), 'private_root': str(private), 'cleanup': receipt['cleanup']}))
raise SystemExit(0 if receipt['result'] == 'passed' else 1)
