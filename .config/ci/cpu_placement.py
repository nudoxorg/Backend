"""Stable integration measurement placement within the worker's existing CPU grant."""
from contextlib import contextmanager
import os
from pathlib import Path


def highest_capacity(allowed: set[int], root: Path = Path('/sys/devices/system/cpu')) -> dict:
    """Use complete, static capacity metadata; never sampled/current frequency."""
    record = {'policy': 'highest-static-capacity', 'allowed': sorted(allowed),
              'selected': sorted(allowed), 'metric': None, 'capacities': {}}
    if not allowed or len(allowed) > 4096:
        record['reason'] = 'empty-or-oversized-cpu-grant'
        return record
    homogeneous = None
    for metric in ('cpu_capacity', 'cpufreq/cpuinfo_max_freq'):
        values = {}
        for cpu in sorted(allowed):
            try:
                with (root/f'cpu{cpu}'/metric).open() as stream:
                    raw = stream.read(65).strip()
                if not raw.isdecimal() or len(raw) > 12 or not 0 < int(raw) <= 10**9:
                    break
                values[cpu] = int(raw)
            except OSError:
                break
        if len(values) == len(allowed):
            maximum = max(values.values())
            candidate = dict(record)
            candidate.update(metric=metric, capacities={str(cpu): value for cpu, value in values.items()},
                          selected=[cpu for cpu, value in values.items() if value == maximum],
                          reason='homogeneous' if len(set(values.values())) == 1 else 'heterogeneous')
            if candidate['reason'] == 'heterogeneous':
                return candidate
            if homogeneous is None:
                homogeneous = candidate
    if homogeneous is not None:
        return homogeneous
    record['reason'] = 'complete-static-capacity-unavailable'
    return record


@contextmanager
def measurement_placement(root: Path = Path('/sys/devices/system/cpu')):
    if not hasattr(os, 'sched_getaffinity') or not hasattr(os, 'sched_setaffinity'):
        yield {'policy': 'highest-static-capacity', 'reason': 'affinity-unavailable',
               'selected': None, 'allowed': None}
        return
    original = set(os.sched_getaffinity(0))
    record = highest_capacity(original, root)
    selected = set(record['selected'])
    changed = selected != original
    try:
        if changed:
            os.sched_setaffinity(0, selected)
            actual = set(os.sched_getaffinity(0))
            if actual != selected:
                raise RuntimeError('integration measurement CPU placement was not applied')
        yield record
    except BaseException as primary:
        if changed:
            try:
                os.sched_setaffinity(0, original)
            except BaseException as restore_error:
                error_type = type(restore_error).__name__
                error_number = getattr(restore_error, 'errno', None)
                detail = error_type
                if error_number is not None:
                    detail = f'{detail} (errno {error_number})'
                primary.add_note(f'failed to restore original CPU affinity: {detail}')
        raise
    else:
        # The exceptional path above has already attempted restoration. Keep
        # the success path separate so a cleanup failure remains visible.
        if changed:
            os.sched_setaffinity(0, original)
