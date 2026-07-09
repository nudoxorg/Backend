"""nudox — shared library for the cargo-like Buck2 third-party tooling.

This package hosts the reusable engine behind the ``crates.py`` dispatcher.
Downstream tooling should depend on the public APIs re-exported by the
individual submodules (see each module's docstring / the project CONTRACT).
"""

from __future__ import annotations

__all__ = [
    "paths",
    "semver",
    "resolve",
    "cratesio",
    "starlark",
    "registry",
    "features",
]
