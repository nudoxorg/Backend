"""Name normalisation, label construction and dependency-label resolution.

Public API
----------
- :func:`normalize` — Rust crate name normalisation (``-`` → ``_``).
- :func:`make_label` — short Buck label (e.g. ``foo-0_2`` / ``bar-1``).
- :func:`build_version_lookup`
- :func:`find_best_matching_label`
- :func:`build_curated_dependency_map`
- :func:`resolve_dependency_label`
- :func:`resolve_sub_dependency_conflicts`
"""

from __future__ import annotations

from collections import defaultdict
from typing import TypedDict

from . import semver


# ── Type aliases (no Any anywhere) ────────────────────────────────────────────
type Registry = list[dict[str, object]]
type VersionLookup = dict[str, list[tuple[str, str]]]
type CuratedMap = dict[tuple[str, str], str]
type LabelSet = set[str]


# ── Core normalisation & labelling ────────────────────────────────────────────
def normalize(name: str) -> str:
    """Rust normalises crate names by turning '-' into '_' for extern names."""
    return name.replace("-", "_")


def make_label(name: str, version: str) -> str:
    """Produce the short label used inside Buck (e.g. 'foo-0_2' or 'bar-1')."""
    parts = version.split(".")
    major = parts[0]
    if major == "0" and len(parts) > 1:
        return f"{normalize(name)}-{major}_{parts[1]}"
    return f"{normalize(name)}-{major}"


def assign_unique_labels(entries: Registry) -> None:
    """Ensure every entry has a unique Buck label (mutates entries in place).

    When multiple versions share the same base label (e.g. ``0.2.2`` and
    ``0.2.3`` both map to ``foo-0_2``), the lowest version keeps the base
    label and later versions get a patch suffix (``foo-0_2_3``).
    """
    by_base: dict[str, list[dict[str, object]]] = defaultdict(list)
    for entry in entries:
        base_label = make_label(str(entry.get("name", "")), str(entry.get("version", "")))
        by_base[base_label].append(entry)

    for base, group in by_base.items():
        if len(group) == 1:
            group[0]["label"] = base
            continue

        sorted_group = sorted(group, key=lambda e: semver.parse(str(e.get("version", "0.0.0"))))
        for index, entry in enumerate(sorted_group):
            if index == 0:
                entry["label"] = base
                continue
            parts = str(entry.get("version", "")).split(".")
            patch = parts[2].split("-")[0] if len(parts) > 2 else "0"
            entry["label"] = f"{base}_{patch}"


# ── Version lookup & best-match resolution ────────────────────────────────────
def build_version_lookup(registry: Registry) -> VersionLookup:
    """name / normalised_name → sorted list of (label, version) pairs."""
    result: dict[str, list[tuple[str, str]]] = defaultdict(list)
    for entry in registry:
        name = str(entry.get("name", ""))
        label = str(entry.get("label", ""))
        version = str(entry.get("version", ""))
        pair = (label, version)
        result[name].append(pair)
        result[normalize(name)].append(pair)
    return dict(result)


def pkg_base_from_label(
    label_str: str, by_label: dict[str, dict[str, object]]
) -> str:
    """Normalised package name for a registry label (``heck-0_4`` → ``heck``)."""
    dep_label = label_str.lstrip(":")
    dep_entry = by_label.get(dep_label, {})
    return normalize(str(dep_entry.get("name", dep_label)))


def find_best_matching_label(
    crate_name: str,
    requirement: str | None,
    lookup: VersionLookup,
) -> str | None:
    """Pick the best (label, version) that satisfies the semver requirement."""
    candidates = lookup.get(crate_name) or lookup.get(normalize(crate_name)) or []
    if not candidates:
        return None

    if not requirement:
        return max(candidates, key=lambda pair: semver.parse(pair[1]))[0]

    if len(candidates) == 1:
        return candidates[0][0]

    matching = [
        (label, ver)
        for label, ver in candidates
        if semver.satisfies(ver, requirement)
    ]
    if matching:
        return max(matching, key=lambda pair: semver.parse(pair[1]))[0]

    # Fallback to first (usually the one already chosen in curated map)
    return candidates[0][0]


def deduplicate_direct_deps(
    dep_labels: list[str], by_label: dict[str, dict[str, object]]
) -> list[str]:
    """Keep at most one direct dep per package name (``heck-0_4`` vs ``heck-0_5``)."""
    best_by_base: dict[str, tuple[str, tuple[int, int, int]]] = {}
    for label_str in dep_labels:
        base = pkg_base_from_label(label_str, by_label)
        dep_entry = by_label.get(label_str.lstrip(":"), {})
        ver = semver.parse(str(dep_entry.get("version", "0.0.0")))
        current = best_by_base.get(base)
        if current is None or ver > current[1]:
            best_by_base[base] = (label_str, ver)

    seen_bases: LabelSet = set()
    deduped: list[str] = []
    for label_str in dep_labels:
        base = pkg_base_from_label(label_str, by_label)
        if base in seen_bases:
            continue
        seen_bases.add(base)
        deduped.append(best_by_base[base][0])
    return deduped


# ── Curated map (source of truth for manual/workspace choices) ────────────────
def build_curated_dependency_map(registry: Registry) -> CuratedMap:
    """(consumer_label, dep_name_or_alias) → chosen dep label.

    This map is the single source of truth for version selection. It is built
    from the already-curated deps + named_deps lists so that workspace = true
    and manual choices are respected.
    """
    by_label = {str(entry.get("label", "")): entry for entry in registry}
    curated: CuratedMap = {}

    for entry in registry:
        consumer_label = str(entry.get("label", ""))
        for dep_label_str in entry.get("deps", []) or []:
            dep_label = str(dep_label_str).lstrip(":")
            dep_entry = by_label.get(dep_label)
            if not dep_entry:
                continue
            dep_name = str(dep_entry.get("name", ""))
            for key in (dep_name, normalize(dep_name)):
                curated.setdefault((consumer_label, key), dep_label)

        for alias, dep_label_str in (entry.get("named_deps") or {}).items():
            dep_label = str(dep_label_str).lstrip(":")
            curated.setdefault((consumer_label, str(alias)), dep_label)

    return curated


# ── Dependency label resolution ───────────────────────────────────────────────
def _buck_pinned_label(
    consumer_label: str,
    dependency_alias: str,
    actual: str,
    buck_dep_pins: dict[tuple[str, str], str] | None,
) -> str | None:
    if not buck_dep_pins:
        return None
    for key in (dependency_alias, actual, normalize(actual), normalize(dependency_alias)):
        pinned = buck_dep_pins.get((consumer_label, key))
        if pinned:
            return pinned
    return None


def resolve_dependency_label(
    consumer_label: str,
    dependency_alias: str,
    dependency_spec: dict[str, object] | str,
    curated: CuratedMap,
    lookup: VersionLookup,
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> str | None:
    """Resolve an alias appearing in a Cargo.toml dep spec to a registry label."""
    match dependency_spec:
        case dict(spec_dict):
            actual = str(spec_dict.get("package", dependency_alias))
            req_ver: str | None = spec_dict.get("version")  # type: ignore[assignment]
        case str() | _:
            actual = dependency_alias
            req_ver = None

    return (
        _buck_pinned_label(consumer_label, dependency_alias, actual, buck_dep_pins)
        or curated.get((consumer_label, actual))
        or curated.get((consumer_label, normalize(actual)))
        or curated.get((consumer_label, dependency_alias))
        or find_best_matching_label(actual, req_ver, lookup)
    )


# ── Sub-dependency conflict resolution (heuristic swap for swappable deps) ────
def resolve_sub_dependency_conflicts(
    dep_labels: list[str],
    swappable: set[str],
    by_label: dict[str, dict[str, object]],
    lookup: VersionLookup,
) -> list[str]:
    """If two direct deps pull in different versions of the same transitive crate,
    and one of the direct deps was added without an explicit version pin
    (i.e. is in `swappable`), try to swap it for another registry candidate
    that removes the conflict.
    """
    result = list(dep_labels)

    def get_sub_dependencies(label_str: str) -> set[str]:
        entry = by_label.get(label_str.lstrip(":"), {})
        return set(str(d) for d in entry.get("deps", []) or [])

    def package_base(label_str: str) -> str:
        entry = by_label.get(label_str.lstrip(":"), {})
        return normalize(str(entry.get("name", label_str.lstrip(":"))))

    # Track visited dep-set states to prevent infinite oscillation on unresolvable conflicts.
    visited: set[frozenset[str]] = {frozenset(result)}

    changed = True
    while changed:
        changed = False

        # Build reverse map: transitive base → set of concrete labels required
        sub_required: dict[str, set[str]] = {}
        for dep_str in result:
            for sub_label in get_sub_dependencies(dep_str):
                sub_required.setdefault(package_base(sub_label), set()).add(sub_label)

        conflicts = {base: labels for base, labels in sub_required.items() if len(labels) > 1}
        if not conflicts:
            break

        for conflict_base, conflict_versions in conflicts.items():
            for dep_str in list(result):
                if dep_str not in swappable:
                    continue
                contributing = get_sub_dependencies(dep_str) & conflict_versions
                if not contributing:
                    continue

                dep_entry = by_label.get(dep_str.lstrip(":"), {})
                dep_name = str(dep_entry.get("name", ""))
                candidates = lookup.get(dep_name) or lookup.get(normalize(dep_name)) or []

                for cand_label, _ in candidates:
                    cand_str = f":{cand_label}"
                    if cand_str == dep_str or cand_str in result:
                        continue
                    if not (get_sub_dependencies(cand_str) & conflict_versions):
                        # Propose the swap
                        candidate_state = frozenset((set(result) - {dep_str}) | {cand_str})
                        if candidate_state in visited:
                            continue  # Would loop — give up on this conflict

                        result.remove(dep_str)
                        result.append(cand_str)
                        swappable.discard(dep_str)
                        swappable.add(cand_str)
                        visited.add(candidate_state)
                        changed = True
                        break
                if changed:
                    break
            if changed:
                break

    return result

