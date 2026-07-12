"""``crates update`` — refresh registry.bzl from crates.io.

Wires the feature-propagation pipeline (:func:`nudox.features.refresh_registry`).
``--reconcile`` / ``--prune`` / ``--check`` are part of the stable CLI surface
but only the refresh path is implemented today.
"""

from __future__ import annotations

import argparse
import re
import sys
from typing import Any

from .. import buckfiles, cratesio, features, registry
from ..resolve import normalize
from . import add


def add_arguments(sub: argparse.ArgumentParser) -> None:
    sub.add_argument(
        "--dry-run",
        action="store_true",
        help="Print result to stdout instead of overwriting registry.bzl",
    )
    sub.add_argument(
        "--reconcile",
        action="store_true",
        help="add crates referenced by BUCK files but missing from the registry",
    )
    sub.add_argument(
        "--no-reconcile",
        action="store_true",
        help="skip the default reconcile pass",
    )
    sub.add_argument(
        "--prune",
        action="store_true",
        help="remove registry entries no longer reachable from any BUCK root",
    )
    sub.add_argument(
        "--check",
        action="store_true",
        help="(not yet implemented) verify the registry is up to date",
    )
    sub.add_argument(
        "--member",
        default=None,
        help="restrict reconcile to a single workspace member's BUCK file",
    )


# ── helpers ────────────────────────────────────────────────────────────────────

def _harvest(args: argparse.Namespace) -> dict[str, set[str]]:
    """Harvest all BUCK files, or just one member's BUCK when --member is set."""
    member = getattr(args, "member", None)
    if member:
        from .. import paths

        # Members live under workspace/<member>/BUCK, except a couple of
        # nested ones; fall back to a recursive search on the member directory.
        candidates = list((paths.WORKSPACE_DIR).rglob("BUCK"))
        wanted = [
            p for p in candidates if member in {part for part in p.parts}
        ]
        if not wanted:
            print(f"  --member {member!r}: no BUCK file found", file=sys.stderr)
            return {"crates": set(), "members": set(), "raw": set(), "unparsed": set()}
        merged: dict[str, set[str]] = {
            "crates": set(), "members": set(), "raw": set(), "unparsed": set()
        }
        for p in wanted:
            one = buckfiles.harvest(p)
            for key in merged:
                merged[key].update(one.get(key, set()))
        return merged
    return buckfiles.harvest_all()


def _should_reconcile(args: argparse.Namespace) -> bool:
    """Default policy: reconcile runs unless explicitly disabled.

    An explicit ``--reconcile`` forces it on; ``--no-reconcile`` forces it off.
    With no narrowing flags, reconcile is the sensible default so ``update``
    keeps the registry in sync with the BUCK files.
    """
    if getattr(args, "no_reconcile", False):
        return False
    return True


def _reachable_labels(
    referenced: dict[str, set[str]],
    reg: list[dict[str, Any]],
    git: list[dict[str, Any]],
) -> set[str]:
    """Compute the closure of registry labels reachable from the BUCK roots.

    Roots are: every referenced alias crate resolved to its registry label,
    every ``raw=[...]`` label of the form ``:label`` (version-pinned targets),
    and every git-crate target (whose own deps also drag in registry labels).
    Edges are the ``deps`` lists (``:label`` strings) on registry + git crates.
    """
    by_label: dict[str, dict[str, Any]] = {e["label"]: e for e in reg}

    # alias name -> registry label.  ``defs.bzl`` emits ``native.alias(name=
    # crate_name, actual=":label")`` for every aliased entry, so deps that
    # reference a bare alias name (e.g. git-crate deps like ``:regex``) must be
    # followed to the real versioned label.
    alias_to_label: dict[str, str] = {}
    for e in reg:
        if e.get("alias"):
            alias_to_label[e.get("crate_name") or normalize(e["name"])] = e["label"]

    # git crate name -> its deps list (git crate targets are their own labels)
    git_deps: dict[str, list[str]] = {}
    for repo in git:
        for c in repo.get("crates", []):
            git_deps[normalize(c["name"])] = c.get("deps", [])

    roots: set[str] = set()
    for name in referenced["crates"]:
        if name in alias_to_label:
            roots.add(alias_to_label[name])
    # raw labels: keep the local-package form (":http-1" -> "http-1"), ignore
    # cross-package (":server-lib", "//...") labels which aren't registry entries.
    for raw in referenced["raw"]:
        if raw.startswith(":"):
            roots.add(raw.lstrip(":"))

    # Seed the git closure too: every git crate is a live root (they are
    # first-party vendored source, never reaped) and pulls in registry labels.
    reachable: set[str] = set()
    stack: list[str] = []

    def _push(label: str) -> None:
        label = label.lstrip(":")
        # A dep may reference a bare alias name rather than the versioned label;
        # resolve it to the real registry label so the walk continues.
        if label not in by_label and label in alias_to_label:
            label = alias_to_label[label]
        stack.append(label)

    for r in roots:
        _push(r)
    for gname in git_deps:
        _push(gname)

    while stack:
        label = stack.pop()
        if label in reachable:
            continue
        reachable.add(label)
        # registry edges
        entry = by_label.get(label)
        if entry:
            for dep in entry.get("deps", []):
                _push(dep)
            for dep in (entry.get("named_deps") or {}).values():
                _push(dep)
        # git-crate edges
        for dep in git_deps.get(label, []):
            _push(dep)

    return reachable


# ── extension points ───────────────────────────────────────────────────────────

def _normalize_version_hint(version: str | None) -> str | None:
    if not version:
        return None
    cleaned = re.sub(r"^[\^~>=<\s*]+", "", version.strip()).split(",")[0].strip()
    if re.match(r"^\d+\.\d+", cleaned):
        return cleaned
    return None


def _reconcile_path_siblings(args: argparse.Namespace) -> list[str]:
    """Add crates.io packages referenced as ``path = ...`` workspace siblings."""
    reg = registry.load_registry()
    existing_names = {e["name"] for e in reg}
    missing: dict[str, str | None] = {}

    for entry in reg:
        manifest = cratesio.fetch_manifest(entry["name"], entry["version"])
        for section in ("dependencies", "build-dependencies"):
            for alias, spec in manifest.get(section, {}).items():
                if not isinstance(spec, dict):
                    continue
                if not spec.get("path"):
                    continue
                actual = spec.get("package", alias)
                if actual in existing_names or actual in missing:
                    continue
                missing[actual] = _normalize_version_hint(spec.get("version"))

    if not missing:
        return []

    existing_versions = {(e["name"], e["version"]) for e in reg}
    raw_collected: dict[str, dict[str, Any]] = {}
    seen: set[str] = set()
    added: list[str] = []

    for name in sorted(missing):
        try:
            before = set(raw_collected)
            add.collect_raw(
                name,
                missing[name],
                existing_names | set(raw_collected),
                raw_collected,
                seen,
                existing_versions,
            )
            if set(raw_collected) - before:
                added.append(name)
        except Exception as exc:  # noqa: BLE001
            print(
                f"warning: could not fetch path sibling '{name}': {exc}",
                file=sys.stderr,
            )

    if not raw_collected:
        return []

    new_entries = add.build_entries(raw_collected, reg)
    sort_key = lambda e: (e["name"].lower(), e["version"])  # noqa: E731
    all_entries = sorted(reg + new_entries, key=sort_key)

    seen_norm: set[str] = set()
    for e in all_entries:
        norm = normalize(e["name"])
        e["alias"] = norm not in seen_norm
        seen_norm.add(norm)

    if not args.dry_run:
        registry.write_registry(all_entries)

    return sorted(set(raw_collected.keys()))


def _reconcile(args: argparse.Namespace) -> list[str]:
    """Add crates referenced by the workspace but missing from the registry.

    Uses the same crates.io seed path as ``crates add`` (``collect_raw`` +
    ``build_entries``) so transitive deps are pulled in too.  Returns the sorted
    list of newly-added crate names.
    """
    if not _should_reconcile(args):
        return []

    reg = registry.load_registry()
    git = registry.load_git()
    referenced = _harvest(args)

    valid = registry.alias_names(reg) | registry.git_crate_names(git)
    missing = sorted(referenced["crates"] - valid)
    if not missing:
        return []

    existing_names = {e["name"] for e in reg}
    existing_versions = {(e["name"], e["version"]) for e in reg}
    raw_collected: dict[str, dict[str, Any]] = {}
    seen: set[str] = set()
    added: list[str] = []

    for name in missing:
        try:
            before = set(raw_collected)
            add.collect_raw(
                name, None, existing_names, raw_collected, seen, existing_versions
            )
            if set(raw_collected) - before:
                added.append(name)
        except Exception as exc:  # noqa: BLE001
            print(
                f"error: could not fetch '{name}' from crates.io: {exc} "
                f"(likely a typo in a BUCK file)",
                file=sys.stderr,
            )
            continue

    if not raw_collected:
        return []

    new_entries = add.build_entries(raw_collected, reg)
    sort_key = lambda e: (e["name"].lower(), e["version"])  # noqa: E731
    all_entries = sorted(reg + new_entries, key=sort_key)

    # Recompute alias flags in final sorted order (mirrors add.run).
    seen_norm: set[str] = set()
    for e in all_entries:
        norm = normalize(e["name"])
        e["alias"] = norm not in seen_norm
        seen_norm.add(norm)

    if not args.dry_run:
        registry.write_registry(all_entries)

    return sorted(set(raw_collected.keys()))


def _prune(args: argparse.Namespace) -> list[str]:
    """Remove registry entries not reachable from any BUCK root.

    Only active when ``args.prune`` is set.  When not pruning we still compute
    the unused set and print it as candidates (informational, no mutation).
    """
    reg = registry.load_registry()
    git = registry.load_git()
    referenced = _harvest(args)

    reachable = _reachable_labels(referenced, reg, git)
    unused = sorted(e["label"] for e in reg if e["label"] not in reachable)

    if not args.prune:
        if unused:
            print(
                f"  {len(unused)} unused registry entr(y/ies) (run with --prune "
                f"to remove): {', '.join(unused)}",
                file=sys.stderr,
            )
        return []

    if not unused:
        return []

    removed_names = sorted(e["name"] for e in reg if e["label"] not in reachable)
    kept = [e for e in reg if e["label"] in reachable]

    if not args.dry_run:
        registry.write_registry(kept)

    return removed_names


def run(args: argparse.Namespace) -> int:
    print("Loading registry...", file=sys.stderr)
    reg = registry.load_registry()
    print(f"  {len(reg)} entries", file=sys.stderr)

    # reconcile BEFORE refresh so newly-added crates get feature propagation
    added = _reconcile(args)
    while True:
        path_added = _reconcile_path_siblings(args)
        if not path_added:
            break
        added.extend(path_added)
    if added:
        # reload so refresh sees the freshly-appended entries
        reg = registry.load_registry()

    harvested = _harvest(args)
    buck_features = buckfiles.buck_features_by_registry_label(harvested, reg)
    buck_dep_pins = buckfiles.buck_dep_pins_by_edge(harvested, reg)
    if buck_features:
        print(
            f"  BUCK-requested features on {len(buck_features)} crate(s)",
            file=sys.stderr,
        )
    if buck_dep_pins:
        consumers = sorted({consumer for consumer, _ in buck_dep_pins})
        print(
            f"  BUCK-requested dep pins on {len(consumers)} consumer(s)",
            file=sys.stderr,
        )
    output = features.refresh_registry(
        reg,
        dry_run=args.dry_run,
        buck_features=buck_features,
        buck_dep_pins=buck_dep_pins,
    )

    # prune AFTER refresh
    removed = _prune(args)

    # cargo-style summary
    if added or removed:
        by_name = {e["name"]: e for e in registry.load_registry()}
        for name in added:
            ver = by_name.get(name, {}).get("version", "?")
            print(f"  Adding {name} v{ver}", file=sys.stderr)
        for name in removed:
            print(f"  Removing {name} (unused)", file=sys.stderr)

    if args.dry_run:
        print(output)
    return 0
