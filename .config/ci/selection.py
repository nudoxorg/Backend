#!/usr/bin/env python3
"""Explain current canonical-PR selection without running tests or accepting snapshots."""
from __future__ import annotations

import argparse
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
DOC_PREFIXES = ('docs/',)
DOC_FILES = ('README.md', 'LICENSE')


def select(paths: list[str] | None, cases: list[str]) -> dict:
    complete = paths is not None
    valid = bool(paths) and all(isinstance(p, str) and p and not p.startswith('/')
                                and '..' not in PurePosixPath(p).parts and '\x00' not in p
                                for p in paths)
    docs_only = valid and all(p in DOC_FILES or p.startswith(DOC_PREFIXES) for p in paths)
    skip = complete and docs_only
    return {
        'schema': 1,
        'policy': 'canonical-pr-rollout-v1',
        'diff_complete': complete and bool(valid),
        'paths': sorted(set(paths)) if valid else [],
        'tier': 'T0' if skip else 'T1+T2',
        'reason': ('Every changed path is explicitly documentation-only.' if skip else
                   'Incomplete or empty diff: run full checks.' if not valid else
                   'Product, configuration or unknown path: run full checks.'),
        'required_contexts': ['concourse/backend-fast'],
        'advisory_contexts': ['concourse/backend-t1', 'concourse/backend-t2'],
        'integration_cases': [] if skip else cases,
        'legacy_platform_lanes': [] if skip else ['windows-wine', 'arm64-emu', 'linux'],
        'scheduled_T3': 'independent nightly/manual schedule',
        'executed': False,
    }


def git(root: Path, *args: str) -> bytes:
    return subprocess.run(['git', '-C', str(root), *args], check=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout


def changed_paths(root: Path, base: str, head: str, worktree: bool) -> tuple[list[str], dict]:
    # Resolve first; user refs never become options or shell code.
    base_sha = git(root, 'rev-parse', '--verify', '--end-of-options', base + '^{commit}').decode().strip()
    head_sha = git(root, 'rev-parse', '--verify', '--end-of-options', head + '^{commit}').decode().strip()
    common = git(root, 'merge-base', base_sha, head_sha).decode().strip()
    paths = git(root, 'diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--name-only', '-z', common, head_sha, '--')
    if worktree:
        if head_sha != git(root, 'rev-parse', 'HEAD').decode().strip():
            raise ValueError('--worktree requires the selected head to be current HEAD')
        # No rename detection retains both deleted source and added destination.
        paths += git(root, 'diff', '--no-ext-diff', '--no-textconv', '--no-renames', '--name-only', '-z', head_sha, '--')
        paths += git(root, 'ls-files', '--others', '--exclude-standard', '-z')
    names = [p.decode('utf-8', errors='surrogateescape') for p in paths.split(b'\x00') if p]
    return sorted(set(names)), {'base_sha': base_sha, 'head_sha': head_sha, 'merge_base': common}


def preview(root: Path, base: str, head: str = 'HEAD', worktree: bool = False) -> dict:
    try:
        catalog = json.loads((root / 'tests/surface/cases.json').read_text())
        cases = [case['name'] for case in catalog['cases']]
        if not cases or len(cases) != len(set(cases)):
            raise ValueError('invalid case catalog')
    except (OSError, ValueError, KeyError, TypeError):
        cases = ['*']  # unknown catalog means all cases, never an empty test set
    try:
        paths, identity = changed_paths(root, base, head, worktree)
        result = select(paths, cases)
        result.update(identity)
    except (OSError, ValueError, subprocess.CalledProcessError):
        result = select(None, cases)
        result['reason'] = 'Diff identity unavailable or incompatible: run full checks; fetch/verify base and head.'
    result['include_worktree'] = worktree
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', required=True)
    parser.add_argument('--head', default='HEAD')
    parser.add_argument('--worktree', action='store_true', help='also include staged, unstaged and untracked files')
    args = parser.parse_args()
    print(json.dumps(preview(ROOT, args.base, args.head, args.worktree), indent=2))
    return 0


if __name__ == '__main__':
    sys.exit(main())
