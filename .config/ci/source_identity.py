"""Share the exact, clean checkout boundary for reports and catalog consumers."""
import re
import subprocess
from pathlib import Path

# T2 checkouts contain many tracked files; allow one full status scan, but
# keep source admission bounded before any product case starts.
TRACKED_STATUS_TIMEOUT_SECONDS = 45


def verify_checkout(root: Path, source_sha: str, label: str = 'CI evidence') -> None:
    if not re.fullmatch(r'[a-f0-9]{40}', source_sha):
        raise ValueError(f'{label} requires a full lower-case source SHA')
    head = subprocess.check_output(['git', '-C', str(root), 'rev-parse', 'HEAD'],
                                   text=True, timeout=10).strip()
    if head != source_sha:
        raise ValueError(f'{label} source differs from the harness checkout')
    dirty = subprocess.check_output(['git', '-C', str(root), 'status', '--porcelain',
                                    '--untracked-files=no'], text=True,
                                   timeout=TRACKED_STATUS_TIMEOUT_SECONDS)
    if dirty.strip():
        raise ValueError(f'{label} requires a clean tracked harness checkout')
