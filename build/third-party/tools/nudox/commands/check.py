"""``crates check`` — offline referential lint for the third-party graph.

Fast, network-free verification that the ``workspace/*/BUCK`` files and
``registry.bzl`` / ``git.bzl`` agree with each other:

- every ``deps(crates=[...])`` name resolves to a registry alias or git crate,
- every ``deps(members=[...])`` is a known workspace member (``_MEMBERS``),
- every ``":label"`` inside a registry/git dep list points at a real target,
- (informational) registry entries not reachable from any BUCK root are flagged
  as garbage-collection candidates.

Returns 0 when clean, 1 when any *hard* problem is found (missing crate ref,
unknown member, dangling dep).  Designed to be called from a pre-commit hook as
``crates.py check --offline``.
"""

from __future__ import annotations

import argparse
import re
import sys
from typing import Any

from .. import buckfiles, paths, registry
from ..resolve import normalize


def add_arguments(sub: argparse.ArgumentParser) -> None:
    sub.add_argument(
        "--offline",
        action="store_true",
        help="do not contact crates.io; use only the local cache",
    )


def _load_members() -> set[str]:
    """Return the set of known workspace member keys from ``_MEMBERS``.

    Exec-with-stubs would require the whole ``rust.bzl`` (and its ``load`` of
    the registry) to run; a targeted regex over the ``_MEMBERS = { ... }`` block
    is simpler, offline, and unambiguous.
    """
    text = paths.RUST_BZL.read_text(encoding="utf-8")
    match = re.search(r"_MEMBERS\s*=\s*\{(.*?)\}", text, re.DOTALL)
    if not match:
        return set()
    block = match.group(1)
    return set(re.findall(r'["\']([A-Za-z0-9_\-]+)["\']\s*:', block))


def _known_targets(
    reg: list[dict[str, Any]], git: list[dict[str, Any]]
) -> set[str]:
    """Every label that a ``:label`` dep is allowed to point at.

    Three flavours of local target exist inside the ``//build/third-party``
    package:

    - the versioned registry label of every entry (e.g. ``serde-1``),
    - the bare alias target that ``defs.bzl`` emits via ``native.alias`` for
      every entry with ``alias == True`` (``crate_name`` or ``normalize(name)``;
      this is what git-crate deps like ``:regex`` / ``:serde`` point at), and
    - every git-crate target, exposed under ``normalize(name)``.
    """
    labels = {e["label"] for e in reg}
    labels |= registry.alias_names(reg)
    labels |= {normalize(c["name"]) for repo in git for c in repo.get("crates", [])}
    return labels


def run(args: argparse.Namespace) -> int:
    offline = getattr(args, "offline", False)

    reg = registry.load_registry()
    git = registry.load_git()
    referenced = buckfiles.harvest_all()

    members = _load_members()
    valid_crates = registry.alias_names(reg) | registry.git_crate_names(git)
    known_targets = _known_targets(reg, git)

    problems: list[str] = []

    # 1. every referenced crate name resolves
    missing_crates = sorted(referenced["crates"] - valid_crates)
    for name in missing_crates:
        problems.append(
            f"missing crate reference: '{name}' is used in a BUCK deps(crates=...) "
            f"but is not a registry alias or git crate"
        )

    # 2. every referenced member is known
    unknown_members = sorted(referenced["members"] - members)
    for name in unknown_members:
        problems.append(
            f"unknown member: deps(members=...) references '{name}', "
            f"known members: {sorted(members)}"
        )

    # 3. registry/git internal integrity: every ":label" dep resolves
    dangling: list[str] = []
    for e in reg:
        for dep in e.get("deps", []):
            target = dep.lstrip(":")
            if target not in known_targets:
                dangling.append(f"{e['label']} -> {dep}")
        for dep in (e.get("named_deps") or {}).values():
            target = dep.lstrip(":")
            if target not in known_targets:
                dangling.append(f"{e['label']} -> {dep} (named_dep)")
    for repo in git:
        for c in repo.get("crates", []):
            src = normalize(c["name"])
            for dep in c.get("deps", []):
                target = dep.lstrip(":")
                if target not in known_targets:
                    dangling.append(f"{src} (git) -> {dep}")
    for d in sorted(set(dangling)):
        problems.append(f"dangling dependency: {d}")

    # 4. INFO: registry entries not reachable from any BUCK root (gc candidates)
    from . import update as _update  # reuse the reachability walk

    reachable = _update._reachable_labels(referenced, reg, git)
    gc_candidates = sorted(e["label"] for e in reg if e["label"] not in reachable)

    # ── report ─────────────────────────────────────────────────────────────
    if problems:
        print(f"check: {len(problems)} problem(s) found:")
        for p in problems:
            print(f"  ERROR: {p}")
    else:
        print("check: no referential problems found")

    if gc_candidates:
        print(
            f"check: {len(gc_candidates)} unused registry entr(y/ies) "
            f"(gc candidates, not a failure):"
        )
        for label in gc_candidates:
            print(f"  INFO: {label} unreachable from any BUCK root")

    if not offline:
        # A full check could additionally shell out to `buck2 build //...` to
        # validate the graph compiles; left as a TODO so the offline path stays
        # the fast, network-free default a pre-commit hook relies on.
        print(
            "check: (non-offline buck2 build verification not implemented; "
            "pass --offline for the fast referential lint)",
            file=sys.stderr,
        )

    return 1 if problems else 0
