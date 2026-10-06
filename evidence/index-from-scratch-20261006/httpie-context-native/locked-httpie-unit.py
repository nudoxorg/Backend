#!/usr/bin/python3
"""Retain Root's allocated compiler permit and certify recent fleet admission."""
import fcntl
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from datetime import datetime, timezone

coordination = Path('/Users/rmccrar6/nudox-functional-corpus-20261006/sol61-coordination')
admission = json.loads(Path(sys.argv[1]).read_text())
if admission.get('advisory_allowed') is not True:
    raise SystemExit('fleet admission refused')
sample_times = [datetime.fromisoformat(host['census']['sample_started_at_utc'].replace('Z', '+00:00'))
                for host in admission['raw_samples'].values()]
age = max((datetime.now(timezone.utc) - value).total_seconds() for value in sample_times)
if age > 60:
    raise SystemExit('fleet admission is older than 60 seconds')
lock_path = coordination / 'compiler-index.lock'
lease = open(lock_path, 'a+')
fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
command = sys.argv[2:]
started = datetime.now(timezone.utc).isoformat()
with (coordination / 'httpie-context-0cf-unit-tests.stdout').open('wb') as stdout, (coordination / 'httpie-context-0cf-unit-tests.stderr').open('wb') as stderr:
    process = subprocess.Popen(command, stdout=stdout, stderr=stderr, start_new_session=True)
    receipt = {'schema': 'nudox.allocated-index-compiler.v1', 'permit_pid': os.getpid(), 'cargo_launcher_pid': process.pid,
               'started': started, 'lock': str(lock_path), 'admission': str(Path(sys.argv[1])), 'admission_age_seconds': age,
               'argv': command, 'allocation': 'Root allocated one private Mac compiler group; explicit -j4'}
    (coordination / 'httpie-context-0cf-unit-tests.launch.json').write_text(json.dumps(receipt, indent=2) + '\n')
    code = process.wait()
receipt.update(exit=code, ended=datetime.now(timezone.utc).isoformat())
(coordination / 'httpie-context-0cf-unit-tests.complete.json').write_text(json.dumps(receipt, indent=2) + '\n')
raise SystemExit(code)
