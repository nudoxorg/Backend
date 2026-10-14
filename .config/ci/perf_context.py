"""Bounded read-only observations; never inputs to performance pass/fail."""
from datetime import datetime, timezone
import json
import math
import os
from pathlib import Path

LIMIT = 2048
FILES = {
    'cpu_max': ('cgroup', 'cpu.max'),
    'cpu_weight': ('cgroup', 'cpu.weight'),
    'cpu_stat': ('cgroup', 'cpu.stat'),
    'cpu_pressure': ('cgroup', 'cpu.pressure'),
    'memory_current': ('cgroup', 'memory.current'),
    'memory_max': ('cgroup', 'memory.max'),
    'memory_events': ('cgroup', 'memory.events'),
    'host_cpu_pressure': ('proc', 'pressure/cpu'),
    'host_io_pressure': ('proc', 'pressure/io'),
}


def read(path: Path, limit: int = LIMIT):
    try:
        with path.open('rb') as stream:
            data = stream.read(limit + 1)
        return {'text': data[:limit].decode('utf-8', errors='replace').strip(),
                'truncated': len(data) > limit}
    except OSError:
        return {'available': False}


def system(proc: Path = Path('/proc'), cgroup: Path = Path('/sys/fs/cgroup')) -> dict:
    result = {'timestamp': datetime.now(timezone.utc).isoformat(),
              'files': {key: read((cgroup if kind == 'cgroup' else proc) / path)
                        for key, (kind, path) in FILES.items()}}
    try:
        allowed = sorted(os.sched_getaffinity(0))
        result['allowed_cpus'] = allowed[:64]
        result['allowed_cpu_count'] = len(allowed)
    except (AttributeError, OSError):
        result['allowed_cpus'] = None
    info = read(proc / 'cpuinfo', 256 * 1024)
    models, frequencies = [], []
    for line in info.get('text', '').splitlines():
        key, separator, value = line.partition(':')
        if not separator:
            continue
        if key.strip() == 'model name' and not models:
            models.append(value.strip()[:160])
        if key.strip() == 'cpu MHz' and len(frequencies) < 64:
            try:
                number = float(value)
                if math.isfinite(number) and 0 < number < 100000:
                    frequencies.append(number)
            except ValueError:
                pass
    result['cpu_model'] = models[0] if models else None
    result['observed_mhz'] = ({'min': min(frequencies), 'max': max(frequencies),
                               'samples': len(frequencies)} if frequencies else None)
    return result


def owner_threads(pid: int, proc: Path = Path('/proc'), ticks: int | None = None) -> dict:
    """Only this case's owner; exited threads are absent from this end sample."""
    ticks = ticks or os.sysconf('SC_CLK_TCK')
    rows = []
    try:
        paths = sorted((proc / str(pid) / 'task').iterdir(), key=lambda p: int(p.name))
    except (OSError, ValueError):
        return {'available': False}
    for path in paths[:256]:
        try:
            raw = (path / 'stat').read_text()
            opening, closing = raw.index('('), raw.rindex(')')
            fields = raw[closing + 1:].split()
            rows.append({'tid': int(path.name), 'name': raw[opening + 1:closing][:160],
                         'cpu_seconds': round((int(fields[11]) + int(fields[12])) / ticks, 2),
                         'state': fields[0]})
        except (OSError, ValueError, IndexError):
            continue
    rows.sort(key=lambda row: (-row['cpu_seconds'], row['tid']))
    return {'available': True, 'sampled': len(rows), 'truncated': len(paths) > 256,
            'top': rows[:10], 'scope': 'Live owner threads at case end; excludes exited threads.'}


def encode(record: dict) -> str:
    """Keep one retained event bounded even when proc data needs JSON escaping."""
    encoded = json.dumps(record, sort_keys=True)
    if len(encoded.encode('utf-8')) <= 65536:
        return encoded
    compact = {**record, 'observations_truncated': True}
    for phase in ('start', 'end'):
        if phase not in record:
            continue
        files = {}
        for name, value in record[phase].get('files', {}).items():
            files[name] = ({**value, 'text': value['text'][:128], 'truncated': True}
                           if 'text' in value else value)
        compact[phase] = {**record[phase], 'files': files}
    encoded = json.dumps(compact, sort_keys=True)
    if len(encoded.encode('utf-8')) <= 65536:
        return encoded
    # Diagnostic collection cannot turn a product pass into a technical failure.
    return json.dumps({key: compact[key] for key in
                       ('schema', 'source_sha', 'artifact_manifest_sha256', 'case', 'observations_truncated')},
                      sort_keys=True)
