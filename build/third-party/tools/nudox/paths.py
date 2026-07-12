"""Well-known repository paths.

All constants are computed once at import time by walking up the directory
tree from this file until the directory containing ``.buckroot`` is found.
That directory is treated as :data:`REPO_ROOT`.
"""

from __future__ import annotations

from pathlib import Path


def _find_repo_root() -> Path:
    """Walk up from this file to the directory containing ``.buckroot``."""
    here = Path(__file__).resolve()
    for candidate in (here, *here.parents):
        if (candidate / ".buckroot").exists():
            return candidate
    # Fallback: this file lives at build/third-party/tools/nudox/paths.py, so the
    # repo root is four levels up.  Keep behaviour sane even without a marker.
    return here.parents[4]


REPO_ROOT: Path = _find_repo_root()

TP_DIR: Path = REPO_ROOT / "build/third-party"
REGISTRY_BZL: Path = TP_DIR / "registry.bzl"
GIT_BZL: Path = TP_DIR / "git.bzl"
CACHE_DIR: Path = TP_DIR / "tools/.registry-cache"

WORKSPACE_DIR: Path = REPO_ROOT / "workspace"
RUST_BZL: Path = REPO_ROOT / "build/rust.bzl"
ROOT_BUCK: Path = REPO_ROOT / "BUCK"
