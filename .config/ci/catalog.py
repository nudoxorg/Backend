"""Expose implemented public test names, optionally bound to exact source bytes."""
import hashlib
import json
from pathlib import Path
import re
import subprocess

from source_identity import verify_checkout

CATALOG_PATH = 'tests/surface/cases.json'


def read_catalog(root: Path, source_sha: str | None = None) -> dict:
    if source_sha is None:
        raw = (root / CATALOG_PATH).read_bytes()
    else:
        verify_checkout(root, source_sha, 'Test catalog')
        # Read the committed blob: untracked files cannot acquire this source identity.
        raw = subprocess.check_output(['git', '-C', str(root), 'show',
                                       f'{source_sha}:{CATALOG_PATH}'], timeout=10, stderr=subprocess.PIPE)
    document = json.loads(raw)
    cases = document.get('cases') if isinstance(document, dict) else None
    if not isinstance(cases, list) or not cases or len(cases) > 200:
        raise ValueError('catalog requires 1–200 integration cases')
    names = [case.get('name') if isinstance(case, dict) else None for case in cases]
    if any(not isinstance(name, str) or not re.fullmatch(r'[a-z0-9][a-z0-9_-]{0,79}', name) for name in names):
        raise ValueError('invalid integration case name')
    if len(names) != len(set(names)):
        raise ValueError('duplicate integration case name')
    tiers = {'smoke': ['cli', 'mcp', 'gui'], 'integration': names,
             'deep': ['cli-index-wait-laws']}
    if source_sha is None:
        return tiers  # preserve the original local list shape
    return {'schema': 1, 'source_sha': source_sha,
            'catalog_sha256': hashlib.sha256(raw).hexdigest(), 'tiers': tiers}
