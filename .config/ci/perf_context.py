"""Bounded read-only observations; never inputs to performance pass/fail."""
from datetime import datetime, timezone
import json
import math
import os
import hashlib
import re
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


def _mount_unescape(value: str) -> str:
    return re.sub(r'\\([0-7]{3})', lambda match: chr(int(match.group(1), 8)), value)


def cgroup_directory(proc: Path, mountpoint: Path) -> tuple[Path | None, dict]:
    """Resolve this process's cgroup inside the visible cgroup2 mount.

    If the proc path cannot be mapped unambiguously to the mount root, report
    the scope as unavailable instead of reading counters from the hierarchy
    root and implying that they belong to this process.
    """
    membership = read(proc / 'self/cgroup', 64 * 1024)
    if 'text' not in membership or membership.get('truncated'):
        return None, {'available': False, 'reason': 'membership-unavailable'}
    paths = [line.split('::', 1)[1] for line in membership['text'].splitlines()
             if '::' in line]
    if len(paths) != 1:
        return None, {'available': False, 'reason': 'membership-ambiguous'}
    process_path = paths[0]
    if (not process_path.startswith('/') or '\x00' in process_path
            or any(part in ('.', '..') for part in process_path.split('/'))):
        return None, {'available': False, 'reason': 'membership-invalid'}

    mountinfo = read(proc / 'self/mountinfo', 256 * 1024)
    if 'text' not in mountinfo or mountinfo.get('truncated'):
        return None, {'available': False, 'reason': 'mountinfo-unavailable'}
    mounts = []
    for line in mountinfo['text'].splitlines():
        before, separator, after = line.partition(' - ')
        if not separator:
            continue
        left, right = before.split(), after.split()
        if len(left) < 5 or not right or right[0] != 'cgroup2':
            continue
        root, visible_mount = map(_mount_unescape, (left[3], left[4]))
        if os.path.normpath(visible_mount) == os.path.normpath(str(mountpoint)):
            mounts.append(root)
    if len(mounts) != 1:
        return None, {'available': False, 'reason': 'cgroup2-mount-unavailable'}

    mount_root = mounts[0]
    if not mount_root.startswith('/') or '\x00' in mount_root:
        return None, {'available': False, 'reason': 'mount-root-invalid'}
    if process_path == '/' or process_path == mount_root:
        relative = ''
    elif mount_root == '/':
        relative = process_path.lstrip('/')
    elif process_path.startswith(mount_root.rstrip('/') + '/'):
        relative = process_path[len(mount_root.rstrip('/')) + 1:]
    else:
        # The mount may be a bind of an unrelated subtree. Guessing that the
        # proc path is namespace-relative would attribute another cgroup's
        # counters to this process.
        return None, {'available': False, 'reason': 'membership-outside-mount-root'}

    directory = mountpoint / relative
    if not directory.is_dir():
        return None, {'available': False, 'reason': 'process-cgroup-missing'}
    members = read(directory / 'cgroup.procs', 256 * 1024)
    if 'text' not in members or str(os.getpid()) not in members['text'].splitlines():
        return None, {'available': False, 'reason': 'process-membership-unverified'}
    return directory, {'available': True, 'path': process_path,
                       'mount_root': mount_root, 'verified_by': 'cgroup.procs'}


def read(path: Path, limit: int = LIMIT):
    try:
        with path.open('rb') as stream:
            data = stream.read(limit + 1)
        return {'text': data[:limit].decode('utf-8', errors='replace').strip(),
                'truncated': len(data) > limit}
    except OSError:
        return {'available': False}


def system(proc: Path = Path('/proc'), cgroup: Path = Path('/sys/fs/cgroup')) -> dict:
    cgroup_dir, scope = cgroup_directory(proc, cgroup)
    result = {'timestamp': datetime.now(timezone.utc).isoformat(),
              'cgroup_scope': scope,
              'files': {key: (read(cgroup_dir / path) if cgroup_dir is not None else {'available': False})
                        if kind == 'cgroup' else read(proc / path)
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
    identity = {'observations_truncated': True}
    schema = compact.get('schema')
    identity['schema'] = schema if type(schema) is int else 1
    for key in ('source_sha', 'artifact_manifest_sha256', 'case'):
        value = compact.get(key)
        if not isinstance(value, str):
            continue
        identity[key] = value[:128]
        if len(value) > 128:
            identity[f'{key}_sha256'] = hashlib.sha256(value.encode('utf-8', errors='surrogatepass')).hexdigest()
            identity[f'{key}_truncated'] = True
    return json.dumps(identity, sort_keys=True)
