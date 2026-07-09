"""``crates build`` / ``crates test`` — dispatch to native buck2.

These verbs are thin wrappers over ``buck2 build`` / ``buck2 test``. Given an
optional workspace ``member`` they resolve it to a package-recursive target
pattern (e.g. ``server`` -> ``//workspace/server/...``); with no member they
target ``//...``.

Member resolution reads ``build/rust.bzl`` and scrapes the ``_MEMBERS`` dict.
We regex the ``"name": "//label"`` pairs instead of importing rust.bzl, because
rust.bzl's top-level ``load(...)`` of registry.bzl/git.bzl only resolves under
Buck2's loader — a plain ``import`` would fail. Each label (``//pkg:target``)
is reduced to its package path and turned into a recursive pattern
(``//pkg/...``) so the whole member subtree is built/tested.

``os.execvp`` replaces this process with buck2 so its exit code propagates
directly to the caller (no wrapping, no lost signals).
"""

from __future__ import annotations

import argparse
import os
import re

from nudox import paths

# Matches   "member":  "//workspace/foo:bar"   inside the _MEMBERS block.
# Skips comment/anchor lines because they carry no quoted label pair.
_MEMBER_RE = re.compile(r'"([^"]+)"\s*:\s*"(//[^"]+)"')


def _load_members() -> dict[str, str]:
    """Return {member_name: label} scraped from the ``_MEMBERS`` dict."""
    text = paths.RUST_BZL.read_text(encoding="utf-8")
    start = text.find("_MEMBERS")
    if start == -1:
        return {}
    brace = text.find("{", start)
    if brace == -1:
        return {}
    # Find the matching closing brace of the dict literal.
    depth = 0
    end = brace
    for i in range(brace, len(text)):
        c = text[i]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                end = i
                break
    block = text[brace : end + 1]
    return {name: label for name, label in _MEMBER_RE.findall(block)}


def _pattern_for(label: str) -> str:
    """`//workspace/server:server-lib` -> `//workspace/server/...`."""
    package = label.split(":", 1)[0]  # drop the :target suffix
    return package.rstrip("/") + "/..."


def _member_patterns() -> dict[str, str]:
    return {name: _pattern_for(label) for name, label in _load_members().items()}


def add_arguments_build(sub: argparse.ArgumentParser) -> None:
    sub.add_argument(
        "member", nargs="?", default=None, help="workspace member to build"
    )


def add_arguments_test(sub: argparse.ArgumentParser) -> None:
    sub.add_argument(
        "member", nargs="?", default=None, help="workspace member to test"
    )


def _resolve_pattern(member: str | None) -> str | None:
    """Map ``member`` (or None) to a buck2 target pattern, or None if unknown."""
    if member is None:
        return "//..."
    patterns = _member_patterns()
    if member not in patterns:
        known = ", ".join(sorted(patterns)) or "(none)"
        print(f"unknown workspace member '{member}'. Known members: {known}")
        return None
    return patterns[member]


def _dispatch(verb: str, member: str | None) -> int:
    pattern = _resolve_pattern(member)
    if pattern is None:
        return 1
    # Replaces this process; exit code of buck2 propagates to the caller.
    os.execvp("buck2", ["buck2", verb, pattern])
    return 1  # unreachable if execvp succeeds


def run_build(args: argparse.Namespace) -> int:
    return _dispatch("build", args.member)


def run_test(args: argparse.Namespace) -> int:
    return _dispatch("test", args.member)
