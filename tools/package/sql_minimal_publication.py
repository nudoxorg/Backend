#!/usr/bin/env python3
"""Private development-mode CLI/owner oracle for mixed Python/SQL publication.

The bundle can contain a matching worker or omit it. A failed oracle preserves
its state and reports reader-lease counts to diagnose publication refusals.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

p = argparse.ArgumentParser()
p.add_argument('--bundle', required=True, type=Path)
p.add_argument('--output', required=True, type=Path)
p.add_argument('--without-worker', action='store_true')
p.add_argument('--wait-seconds', type=int, default=120)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=False)
a.output = a.output.resolve()
private = Path(tempfile.mkdtemp(prefix='min-', dir=a.output.parent))
bundle = private / 'bundle'
bundle.mkdir()
identities = {}
for name in ('backend-cli', 'backend-locald', 'backend-mcp', 'backend-sqlite-worker'):
    if name == 'backend-sqlite-worker' and a.without_worker:
        continue
    source = (a.bundle / name).resolve()
    if not source.is_file():
        if name in ('backend-mcp', 'backend-sqlite-worker'):
            continue
        raise SystemExit(f'missing {name}')
    digest = hashlib.sha256()
    with source.open('rb') as binary:
        for block in iter(lambda: binary.read(1024 * 1024), b''):
            digest.update(block)
    identities[name] = digest.hexdigest()
    os.link(source, bundle / name)
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
receipt = {'binaries': identities, 'development_mode': True, 'state': str(state), 'project': str(project), 'commands': []}

def cli(*args):
    command = [str(bundle / 'backend-cli'), '--format', 'json', '--project', str(project), '--workspace', str(state), '--endpoint', str(endpoint), *args]
    start = time.monotonic()
    result = subprocess.run(command, cwd=project, env=env, capture_output=True, timeout=a.wait_seconds + 120)
    record = {'argv': command, 'exit': result.returncode, 'seconds': time.monotonic() - start, 'stdout': result.stdout.decode(errors='replace'), 'stderr': result.stderr.decode(errors='replace')}
    receipt['commands'].append(record)
    return record

owner = None
try:
    for turn in range(2):
        if endpoint.exists():
            endpoint.unlink()
        owner = subprocess.Popen([str(bundle / 'backend-locald'), '--endpoint', str(endpoint), '--workspace', str(state), '--authority-secret-file', str(secret), '--registry-offline', '--registry-discovery-offline', '--advisory-offline', '--forge-offline'], cwd=project, env=env, stdout=subprocess.DEVNULL, stderr=(a.output / f'owner-{turn}.stderr').open('wb'), start_new_session=True)
        deadline = time.monotonic() + 120
        while not endpoint.exists():
            if owner.poll() is not None or time.monotonic() > deadline:
                raise RuntimeError('owner failed to start')
            time.sleep(0.05)
        if turn == 0:
            index = cli('add', str(project), '--wait', '--wait-timeout', str(a.wait_seconds))
            receipt['lease_directories'] = {str(path.relative_to(state)): len(list(path.iterdir())) for path in state.rglob('*') if path.is_dir() and 'lease' in path.name}
            if index['exit'] != 0:
                cli('status')
                raise RuntimeError(f"mixed publication failed: exit {index['exit']}")
        gap = cli('sql-query', str(project), '--role', 'gap')
        if gap['exit'] != 0:
            raise RuntimeError('SQL gap query failed')
        os.killpg(owner.pid, signal.SIGKILL)
        owner.wait(timeout=10)
        owner = None
    receipt['result'] = 'passed'
except Exception as error:
    receipt['result'] = 'failed'
    receipt['error'] = str(error)
finally:
    if owner is not None and owner.poll() is None:
        os.killpg(owner.pid, signal.SIGTERM)
        try:
            owner.wait(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(owner.pid, signal.SIGKILL)
            owner.wait(timeout=10)
    (a.output / 'receipt.json').write_text(json.dumps(receipt, indent=2))
print(json.dumps({'result': receipt['result'], 'output': str(a.output), 'state': str(state)}))
raise SystemExit(0 if receipt['result'] == 'passed' else 1)
