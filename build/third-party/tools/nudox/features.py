"""Version-aware feature propagation — the full gen-registry engine.

Public API
----------
- :func:`refresh_registry` — run the complete pipeline (fetch manifests,
  propagate features to a fixed point, enrich deps, detect lib/proc-macro
  overrides) and return the full registry.bzl text. Writes it unless
  ``dry_run=True``.

Internal (moved verbatim from gen-registry.py, kept importable):
  :func:`expand_feature_set`, :func:`iter_all_dependency_specs`,
  :func:`activate_optional_dependencies`, :func:`propagate_feature_activations`,
  :func:`enrich_dependency_list`, :func:`detect_library_overrides`.
"""

from __future__ import annotations

import sys
from collections import deque
from collections.abc import Iterator, Mapping
from typing import TypedDict

from . import cratesio, paths, starlark
from .resolve import (
    assign_unique_labels,
    build_curated_dependency_map,
    build_version_lookup,
    deduplicate_direct_deps,
    find_best_matching_label,
    normalize,
    pkg_base_from_label,
    resolve_dependency_label,
    resolve_sub_dependency_conflicts,
    _buck_pinned_label,
)


# ── Typed structures for manifests (avoid Any entirely) ────────────────────────
class DependencySpecification(TypedDict, total=False):
    """Schema for a dependency table entry (when not a bare version string)."""

    version: str
    optional: bool
    features: list[str]
    package: str
    path: str
    git: str
    workspace: bool
    default_features: bool


Manifest = dict[str, object]
Registry = list[dict[str, object]]
FeatureSet = set[str]
DependencyMap = dict[str, DependencySpecification]


# ── Safe accessors for heterogeneous manifest dicts (robustness) ──────────────
def _get_str(data: object, key: str, default: str | None = None) -> str | None:
    """Safely extract a string value from a dict-like object."""
    if isinstance(data, dict):
        value = data.get(key)
        if isinstance(value, str):
            return value
    return default


def _get_bool(data: object, key: str, default: bool = False) -> bool:
    """Safely extract a boolean value from a dict-like object."""
    if isinstance(data, dict):
        value = data.get(key)
        if isinstance(value, bool):
            return value
    return default


def _get_str_list(data: object, key: str) -> list[str]:
    """Safely extract a list of strings from a dict-like object."""
    if isinstance(data, dict):
        value = data.get(key)
        if isinstance(value, list):
            return [item for item in value if isinstance(item, str)]
    return []


def _get_dict(data: object, key: str) -> dict[str, object]:
    """Safely extract a dict from a dict-like object."""
    if isinstance(data, dict):
        value = data.get(key)
        if isinstance(value, dict):
            return value  # type: ignore[return-value]
    return {}


# ── feature expansion (transitive closure) ────────────────────────────────────
def expand_feature_set(
    feature_definitions: Mapping[str, list[str]], seeds: set[str]
) -> set[str]:
    """Return the transitive closure of features reachable from seeds.

    Ignores "dep:..." and "crate/feature" items (those are handled elsewhere).
    """
    expanded: set[str] = set(seeds)
    queue: deque[str] = deque(seeds)
    while queue:
        feature = queue.popleft()
        for sub_feature in feature_definitions.get(feature, []):
            if sub_feature.startswith("dep:") or "/" in sub_feature:
                continue
            if sub_feature not in expanded and sub_feature in feature_definitions:
                expanded.add(sub_feature)
                queue.append(sub_feature)
    return expanded


# ── unified dependency iterator (DRY) ─────────────────────────────────────────
def _merge_dependency_specification(
    existing: DependencySpecification | None, incoming: DependencySpecification
) -> DependencySpecification:
    """Merge two manifests for the same dependency alias.

    Target-specific tables (e.g. ``[target.'cfg(...)'.dependencies]``) often
    repeat a crate without the ``optional`` flag. Later entries must not
    clobber an earlier ``optional = true`` from the root ``[dependencies]``.
    """
    if existing is None:
        return dict(incoming)  # shallow copy is fine
    merged: DependencySpecification = dict(existing)
    if incoming.get("optional"):
        merged["optional"] = True
    for key, value in incoming.items():
        if key not in merged or merged.get(key) in (None, ""):
            merged[key] = value  # type: ignore[assignment]
    return merged


def iter_all_dependency_specs(
    manifest: Mapping[str, object],
) -> Iterator[tuple[str, str | DependencySpecification]]:
    """Yield every (alias, spec) from dependencies, build-dependencies and
    all target.'cfg(...)'. sections. Spec may be a version string or a dict.
    """
    for section in ("dependencies", "build-dependencies"):
        section_dict = _get_dict(manifest, section)
        for alias, spec in section_dict.items():
            if isinstance(spec, (str, dict)):
                yield alias, spec  # type: ignore[misc]
    target_section = _get_dict(manifest, "target")
    for target_block in target_section.values():
        if isinstance(target_block, dict):
            for section in ("dependencies", "build-dependencies"):
                section_dict = target_block.get(section, {})
                if isinstance(section_dict, dict):
                    for alias, spec in section_dict.items():
                        if isinstance(spec, (str, dict)):
                            yield alias, spec  # type: ignore[misc]


# ── optional-dependency activation ────────────────────────────────────────────
def activate_optional_dependencies(
    manifest: Mapping[str, object], active_features: set[str]
) -> DependencyMap:
    """Return {dep_alias: spec_dict} for every optional dep that should be on.

    Implements Cargo's activation rules (including the "feature name == dep name"
    legacy rule and the fixed-point behaviour when activating a dep also
    activates a feature of the same name).
    """
    feature_definitions: dict[str, list[str]] = _get_feature_definitions(manifest)
    all_dependencies: DependencyMap = {}
    for alias, spec in iter_all_dependency_specs(manifest):
        if isinstance(spec, dict):
            all_dependencies[alias] = _merge_dependency_specification(
                all_dependencies.get(alias), spec  # type: ignore[arg-type]
            )

    optional_names = {alias for alias, spec in all_dependencies.items() if spec.get("optional")}

    activated: DependencyMap = {}
    pending_features: set[str] = set(active_features)
    changed = True
    while changed:
        changed = False
        for feature in list(pending_features):
            # Legacy: feature name that matches an optional dep name
            if feature in optional_names and feature not in activated:
                activated[feature] = all_dependencies[feature]
                changed = True
                if feature in feature_definitions and feature not in pending_features:
                    pending_features.add(feature)

            for item in feature_definitions.get(feature, []):
                alias: str | None = None
                if item.startswith("dep:"):
                    alias = item[4:]
                elif item in optional_names:
                    alias = item
                elif "/" in item:
                    dep_part = item.split("/", 1)[0].rstrip("?")
                    if dep_part in optional_names:
                        alias = dep_part
                if alias and alias in all_dependencies and alias not in activated:
                    activated[alias] = all_dependencies[alias]
                    changed = True
                    if alias in feature_definitions and alias not in pending_features:
                        pending_features.add(alias)
    return activated


def _get_feature_definitions(manifest: Mapping[str, object]) -> dict[str, list[str]]:
    """Extract features table robustly as {feature_name: [sub_features]}."""
    raw_features = manifest.get("features")
    if not isinstance(raw_features, dict):
        return {}
    result: dict[str, list[str]] = {}
    for feature_name, items in raw_features.items():
        if isinstance(feature_name, str):
            result[feature_name] = _get_str_list({"items": items}, "items")
    return result


# ── core propagation engine ───────────────────────────────────────────────────
def propagate_feature_activations(
    registry: Registry,
    manifest_cache: dict[str, Manifest],
    lookup: dict[str, list[tuple[str, str]]],
    buck_features: dict[str, list[str]] | None = None,
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> dict[str, set[str]]:
    """Fixed-point propagation of features through the dependency graph.

    ``buck_features`` is keyed by registry label (canonical alias version only).
    Version resolution prefers the version already recorded in the curated
    deps list (so manual/workspace choices win), then falls back to semver.
    """
    by_label: dict[str, dict[str, object]] = {entry["label"]: entry for entry in registry}  # type: ignore[index]
    curated = build_curated_dependency_map(registry)

    # Seed with each crate's own default features + pinned overrides
    activated: dict[str, set[str]] = {}
    for entry in registry:
        key = f"{entry['name']}-{entry['version']}"  # type: ignore[index]
        manifest = manifest_cache.get(key, {})
        feature_definitions = _get_feature_definitions(manifest)
        defaults = set(feature_definitions.get("default", []))
        requested = set((buck_features or {}).get(str(entry.get("label", "")), []))
        seeds = requested if requested else defaults
        activated[entry["label"]] = expand_feature_set(feature_definitions, seeds)  # type: ignore[index]

    changed = True
    while changed:
        changed = False
        for entry in registry:
            key = f"{entry['name']}-{entry['version']}"  # type: ignore[index]
            manifest = manifest_cache.get(key, {})
            feature_definitions = _get_feature_definitions(manifest)

            # A) features = [...] declared directly on a dependency
            for dep_alias, raw_spec in iter_all_dependency_specs(manifest):
                if not isinstance(raw_spec, dict):
                    continue
                requested_features = _get_str_list(raw_spec, "features")
                if not requested_features:
                    continue
                dependency_label = resolve_dependency_label(
                    str(entry["label"]),
                    dep_alias,
                    raw_spec,
                    curated,
                    lookup,
                    buck_dep_pins,
                )
                if not dependency_label or dependency_label not in activated:
                    continue
                dep_entry = by_label[dependency_label]
                dep_key = f"{dep_entry['name']}-{dep_entry['version']}"  # type: ignore[index]
                dep_manifest = manifest_cache.get(dep_key, {})
                new_features = expand_feature_set(
                    _get_feature_definitions(dep_manifest), set(requested_features)
                )
                before = frozenset(activated[dependency_label])
                activated[dependency_label] |= new_features
                if frozenset(activated[dependency_label]) != before:
                    changed = True

            # B) "dep/feature" syntax inside feature definitions
            dep_specs: dict[str, str | DependencySpecification] = dict(
                iter_all_dependency_specs(manifest)
            )
            for feat, items in feature_definitions.items():
                if feat not in activated.get(entry["label"], set()):  # type: ignore[index]
                    continue
                for item in items:
                    if "/" not in item:
                        continue
                    dep_alias, sub_feat = item.split("/", 1)
                    dep_alias = dep_alias.rstrip("?")
                    raw_spec = dep_specs.get(dep_alias, {})
                    dependency_label = resolve_dependency_label(
                        str(entry["label"]),
                        dep_alias,
                        raw_spec,
                        curated,
                        lookup,
                        buck_dep_pins,
                    )
                    if not dependency_label or dependency_label not in activated:
                        continue
                    dep_entry = by_label[dependency_label]
                    dep_key = f"{dep_entry['name']}-{dep_entry['version']}"  # type: ignore[index]
                    dep_manifest = manifest_cache.get(dep_key, {})
                    new_features = expand_feature_set(
                        _get_feature_definitions(dep_manifest), {sub_feat}
                    )
                    before = frozenset(activated[dependency_label])
                    activated[dependency_label] |= new_features
                    if frozenset(activated[dependency_label]) != before:
                        changed = True

    return activated


# ── Dependency parsing helpers (eliminates massive duplication) ───────────────
def _parse_dependency_entry(
    alias: str, raw_spec: str | object
) -> tuple[str, str | None, bool, bool] | None:
    """Return (actual_package, required_version, is_optional, is_git) or None."""
    match raw_spec:
        case str(version):
            return alias, version, False, False
        case dict(spec_dict):
            is_optional = _get_bool(spec_dict, "optional")
            is_git = bool(_get_str(spec_dict, "git"))
            actual = _get_str(spec_dict, "package", alias) or alias
            required_version = _get_str(spec_dict, "version")
            if _get_str(spec_dict, "path") and not required_version:
                return None
            return actual, required_version, is_optional, is_git
        case _:
            return None


def _iter_root_dependency_specs(
    manifest: Mapping[str, object],
) -> Iterator[tuple[str, str | DependencySpecification]]:
    """Yield deps from root ``[dependencies]`` / ``[build-dependencies]`` only."""
    for section in ("dependencies", "build-dependencies"):
        section_dict = _get_dict(manifest, section)
        for alias, spec in section_dict.items():
            if isinstance(spec, (str, dict)):
                yield alias, spec  # type: ignore[misc]


def _iter_direct_non_optional_dependencies(
    manifest: Mapping[str, object],
) -> Iterator[tuple[str, str, str | None]]:
    """Yield (alias, actual_package, required_version) for non-optional non-git deps."""
    for alias, raw_spec in _iter_root_dependency_specs(manifest):
        parsed = _parse_dependency_entry(alias, raw_spec)
        if parsed is None:
            continue
        actual, required_version, is_optional, is_git = parsed
        if is_optional or is_git:
            continue
        yield alias, actual, required_version


# ── dependency enrichment & feature filtering ─────────────────────────────────
def _align_required_dependency_versions(
    dependency_labels: list[str],
    manifest: Mapping[str, object],
    lookup: dict[str, list[tuple[str, str]]],
    by_label: dict[str, dict[str, object]],
    consumer_label: str = "",
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> list[str]:
    """Ensure each required manifest dep uses the best registry label."""
    labels = list(dependency_labels)
    label_set = set(labels)
    for alias, actual, req_version in _iter_direct_non_optional_dependencies(manifest):
        best = (
            _buck_pinned_label(consumer_label, alias, actual, buck_dep_pins)
            or find_best_matching_label(actual, req_version, lookup)
        )
        if not best:
            continue
        best_str = f":{best}"
        base = normalize(actual)
        for existing in list(label_set):
            if (
                pkg_base_from_label(existing, by_label) == base
                and existing != best_str
            ):
                labels.remove(existing)
                label_set.discard(existing)
        if best_str not in label_set:
            labels.append(best_str)
            label_set.add(best_str)
    return labels


def _required_dependency_labels(
    manifest: Mapping[str, object],
    lookup: dict[str, list[tuple[str, str]]],
    existing_labels: set[str],
    by_label: dict[str, dict[str, object]],
    consumer_label: str = "",
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> list[str]:
    """Resolve non-optional root [dependencies] and [build-dependencies]."""
    existing_bases = {
        pkg_base_from_label(label_str, by_label) for label_str in existing_labels
    }
    labels: list[str] = []
    for alias, actual, req_version in _iter_direct_non_optional_dependencies(manifest):
        if normalize(actual) in existing_bases:
            continue
        dependency_label = (
            _buck_pinned_label(consumer_label, alias, actual, buck_dep_pins)
            or find_best_matching_label(actual, req_version, lookup)
        )
        if dependency_label:
            labels.append(f":{dependency_label}")
            existing_bases.add(normalize(actual))
    return labels


def _required_named_dependencies(
    manifest: Mapping[str, object],
    lookup: dict[str, list[tuple[str, str]]],
    by_label: dict[str, dict[str, object]],
    consumer_label: str = "",
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> dict[str, str]:
    """Return ``{alias: :label}`` for renamed non-optional manifest deps."""
    named: dict[str, str] = {}
    for alias, actual, req_version in _iter_direct_non_optional_dependencies(manifest):
        dependency_label = (
            _buck_pinned_label(consumer_label, alias, actual, buck_dep_pins)
            or find_best_matching_label(actual, req_version, lookup)
        )
        if not dependency_label:
            continue
        dep_entry = by_label.get(dependency_label, {})
        dep_effective = dep_entry.get("crate_name") or normalize(
            str(dep_entry.get("name", actual))
        )
        if normalize(alias) != dep_effective:
            named.setdefault(normalize(alias), f":{dependency_label}")
    return named


def _is_host_target_key(target_key: str) -> bool:
    """True for target tables that apply to native host builds (not wasm/wasi/uefi)."""
    key = str(target_key)
    if any(
        token in key
        for token in (
            "unix",
            "windows",
            "macos",
            "linux",
            "freebsd",
            "openbsd",
            "netbsd",
            "android",
            "ios",
            "dragonfly",
            "solaris",
            "cygwin",
            "horizon",
            "haiku",
            "redox",
            "aix",
            "vxworks",
            "vita",
            "nto",
            "hurd",
            "illumos",
        )
    ):
        return True
    # cfg(not(...)) tables apply everywhere except the inner condition — host builds included.
    if "not(" in key or "not (" in key:
        return True
    return not any(
        token in key
        for token in (
            "wasm32",
            "wasm64",
            "wasi",
            "uefi",
            "emscripten",
            'target_os = "unknown"',
            'target_os = "none"',
        )
    )


def _host_target_dependency_labels(
    manifest: Mapping[str, object],
    lookup: dict[str, list[tuple[str, str]]],
    consumer_label: str = "",
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> list[str]:
    """Resolve non-optional deps from host-relevant ``[target.'cfg(...)']`` tables."""
    labels: list[str] = []
    target_section = _get_dict(manifest, "target")
    for target_key, block in target_section.items():
        if not isinstance(block, dict):
            continue
        if not _is_host_target_key(str(target_key)):
            continue
        for section in ("dependencies", "build-dependencies"):
            deps_section = _get_dict(block, section)
            for alias, raw_spec in deps_section.items():
                parsed = _parse_dependency_entry(alias, raw_spec)
                if parsed is None:
                    continue
                actual, req_version, is_optional, is_git = parsed
                if is_optional or is_git:
                    continue
                dependency_label = (
                    _buck_pinned_label(consumer_label, alias, actual, buck_dep_pins)
                    or find_best_matching_label(actual, req_version, lookup)
                )
                if dependency_label:
                    labels.append(f":{dependency_label}")
    return labels


def enrich_dependency_list(
    entry: dict[str, object],
    manifest: Mapping[str, object],
    active_features: set[str],
    lookup: dict[str, list[tuple[str, str]]],
    by_label: dict[str, dict[str, object]],
    buck_features: dict[str, list[str]] | None = None,
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> tuple[list[str], dict[str, str], set[str]]:
    """Return (dep_labels, named_deps, filtered_features) after enrichment."""
    consumer_label = str(entry.get("label", ""))

    # Collect optional dep specs from root tables only (never seed from stale registry deps).
    all_optional: DependencyMap = {}
    for alias, spec in _iter_root_dependency_specs(manifest):
        if isinstance(spec, dict):
            all_optional[alias] = _merge_dependency_specification(
                all_optional.get(alias), spec  # type: ignore[arg-type]
            )

    dependency_labels: list[str] = []
    cleaned_existing: set[str] = set()
    existing_name_to_label: dict[str, str] = {}
    named_dependencies: dict[str, str] = {}

    for label_str in _required_dependency_labels(
        manifest, lookup, cleaned_existing, by_label, consumer_label, buck_dep_pins
    ):
        dependency_labels.append(label_str)
        cleaned_existing.add(label_str)
        dep_label = label_str.lstrip(":")
        dep_entry = by_label.get(dep_label, {})
        name = str(dep_entry.get("name", dep_label))
        existing_name_to_label[name] = dep_label
        existing_name_to_label[normalize(name)] = dep_label

    for label_str in _host_target_dependency_labels(
        manifest, lookup, consumer_label, buck_dep_pins
    ):
        if label_str in cleaned_existing:
            continue
        dependency_labels.append(label_str)
        cleaned_existing.add(label_str)
        dep_label = label_str.lstrip(":")
        dep_entry = by_label.get(dep_label, {})
        name = str(dep_entry.get("name", dep_label))
        existing_name_to_label[name] = dep_label
        existing_name_to_label[normalize(name)] = dep_label

    for alias, label_str in _required_named_dependencies(
        manifest, lookup, by_label, consumer_label, buck_dep_pins
    ).items():
        named_dependencies[alias] = label_str

    # Activate optional deps that the current feature set turns on
    missing_dependency_aliases: set[str] = set()
    swappable: set[str] = set()
    for dep_alias, dep_spec in activate_optional_dependencies(
        manifest, active_features
    ).items():
        if dep_spec.get("git"):
            continue
        actual = str(dep_spec.get("package", dep_alias) or dep_alias)
        required_version = dep_spec.get("version")
        dependency_label = (
            _buck_pinned_label(consumer_label, dep_alias, actual, buck_dep_pins)
            or existing_name_to_label.get(actual)
            or existing_name_to_label.get(normalize(actual))
            or find_best_matching_label(actual, required_version, lookup)
        )
        if not dependency_label:
            missing_dependency_aliases.add(dep_alias)
            continue
        dep_label_str = f":{dependency_label}"
        dep_entry = by_label.get(dependency_label, {})
        dep_effective = str(
            dep_entry.get("crate_name") or normalize(str(dep_entry.get("name", actual)))
        )
        is_rename = normalize(dep_alias) != dep_effective
        if dep_label_str in cleaned_existing:
            if is_rename:
                if dep_label_str in dependency_labels:
                    dependency_labels.remove(dep_label_str)
                cleaned_existing.discard(dep_label_str)
                named_dependencies.setdefault(dep_alias, dep_label_str)
            continue
        if is_rename:
            named_dependencies.setdefault(dep_alias, dep_label_str)
        else:
            dependency_labels.append(dep_label_str)
            if not required_version or dep_spec.get("workspace"):
                swappable.add(dep_label_str)

    # Try to fix sub-dep version conflicts caused by un-pinned deps
    dependency_labels = resolve_sub_dependency_conflicts(
        dependency_labels, swappable, by_label, lookup
    )

    dependency_labels = _align_required_dependency_versions(
        dependency_labels, manifest, lookup, by_label, consumer_label, buck_dep_pins
    )
    dependency_labels = deduplicate_direct_deps(dependency_labels, by_label)

    inactive_optional_bases: set[str] = set()
    active_optional = activate_optional_dependencies(manifest, active_features)
    activated_optional_bases = {
        normalize(str(spec.get("package", alias) or alias))
        for alias, spec in active_optional.items()
    }
    for alias, spec in iter_all_dependency_specs(manifest):
        if not isinstance(spec, dict) or not spec.get("optional"):
            continue
        if alias in active_optional:
            continue
        actual = str(spec.get("package", alias) or alias)
        base = normalize(actual)
        # Multiple version-specific optional aliases (e.g. hashbrown-0_14 vs
        # hashbrown) share a package base — only drop the base when *no*
        # activated optional targets that package.
        if base not in activated_optional_bases:
            inactive_optional_bases.add(base)

    if inactive_optional_bases:
        dependency_labels = [
            label_str
            for label_str in dependency_labels
            if pkg_base_from_label(label_str, by_label) not in inactive_optional_bases
        ]

    if named_dependencies:
        renamed_bases = {normalize(alias) for alias in named_dependencies}
        kept_labels: list[str] = []
        for label_str in dependency_labels:
            base = pkg_base_from_label(label_str, by_label)
            if base in renamed_bases and label_str not in named_dependencies.values():
                continue
            kept_labels.append(label_str)
        dependency_labels = kept_labels

    # Cargo implicitly enables cfg(feature = "dep_alias") for every activated
    # optional dependency. Replicate that so downstream #[cfg(feature = "...")]
    # continues to work.
    activated_aliases = set(activate_optional_dependencies(manifest, active_features).keys())
    extra_features = activated_aliases - missing_dependency_aliases - active_features
    working_features = active_features | extra_features

    # Drop any feature (transitively) that would pull in a missing optional dep,
    # except features declared in workspace BUCK files via deps(features={...})
    pinned = set((buck_features or {}).get(str(entry.get("label", "")), []))
    feature_definitions = _get_feature_definitions(manifest)
    to_drop: set[str] = set()
    for feature in working_features:
        if feature in missing_dependency_aliases and feature not in pinned:
            to_drop.add(feature)
            continue
        for item in feature_definitions.get(feature, []):
            alias = item[4:] if item.startswith("dep:") else item
            if alias in missing_dependency_aliases and feature not in pinned:
                to_drop.add(feature)
                break

    changed = True
    while changed:
        changed = False
        for feature in working_features - to_drop:
            if feature in pinned:
                continue
            for item in feature_definitions.get(feature, []):
                item_name = item[4:] if item.startswith("dep:") else item
                if item_name in to_drop:
                    to_drop.add(feature)
                    changed = True
                    break

    filtered_features = (working_features - to_drop) | pinned

    # postcard's `alloc` feature optionally enables *one* embedded-io crate;
    # propagating both optional deps is a host-build footgun.
    if str(entry.get("name")) == "postcard" and {
        "embedded-io-04",
        "embedded-io-06",
    } <= set(filtered_features):
        filtered_features -= {"embedded-io-04", "embedded-io-06"}
        for alias in ("embedded-io-04", "embedded-io-06"):
            named_dependencies.pop(alias, None)

    return sorted(set(dependency_labels)), named_dependencies, filtered_features


# ── lib metadata detection ────────────────────────────────────────────────────
def detect_library_overrides(
    crate_name: str, manifest: Mapping[str, object]
) -> tuple[str | None, str | None]:
    """Return (lib_root, crate_name) when the crate uses a non-standard layout."""
    lib_section = _get_dict(manifest, "lib")
    path = _get_str(lib_section, "path")
    if path:
        path = path.replace("\\", "/")
        if path == "src/lib.rs":
            path = None
    lib_name = _get_str(lib_section, "name")
    if lib_name and normalize(lib_name) == normalize(crate_name):
        lib_name = None
    return path, lib_name


# ── top-level pipeline ────────────────────────────────────────────────────────
def refresh_registry(
    registry: Registry,
    *,
    dry_run: bool = False,
    buck_features: dict[str, list[str]] | None = None,
    buck_dep_pins: dict[tuple[str, str], str] | None = None,
) -> str:
    """Run the full feature-propagation pipeline and return registry.bzl text.

    Fetches manifests, propagates features to a fixed point, enriches deps,
    detects lib overrides / proc-macro flags, and produces the complete
    registry.bzl text. When ``dry_run`` is False the text is also written to
    :data:`paths.REGISTRY_BZL`.
    """
    assign_unique_labels(registry)
    lookup = build_version_lookup(registry)
    by_label: dict[str, dict[str, object]] = {entry["label"]: entry for entry in registry}  # type: ignore[index]

    print("Fetching Cargo.toml files (cached after first run)...", file=sys.stderr)
    manifest_cache: dict[str, Manifest] = {}
    build_script_cache: dict[str, bool] = {}
    total = len(registry)
    for i, entry in enumerate(registry):
        key = f"{entry['name']}-{entry['version']}"  # type: ignore[index]
        progress = f"\r [{i + 1:>{len(str(total))}}/{total}] {str(entry.get('name', '')):<45}"
        print(progress, end="", file=sys.stderr, flush=True)
        manifest, has_build = cratesio.fetch_manifest_meta(
            str(entry["name"]), str(entry["version"])  # type: ignore[arg-type]
        )
        manifest_cache[key] = manifest
        build_script_cache[key] = has_build
    print(file=sys.stderr)

    print("Propagating features (version-aware, fixed-point)...", file=sys.stderr)
    activated = propagate_feature_activations(
        registry, manifest_cache, lookup, buck_features, buck_dep_pins
    )

    print(
        "Detecting lib overrides, proc-macro flags, enriching optional deps...",
        file=sys.stderr,
    )

    # First pass: structural metadata that does not depend on feature sets
    for entry in registry:
        key = f"{entry['name']}-{entry['version']}"  # type: ignore[index]
        manifest = manifest_cache.get(key, {})
        detected_root, detected_crate = detect_library_overrides(
            str(entry.get("name", "")), manifest
        )
        if detected_root and not entry.get("lib_root"):
            entry["lib_root"] = detected_root
        if detected_crate and not entry.get("crate_name"):
            entry["crate_name"] = detected_crate

        package_section = _get_dict(manifest, "package")
        links_name = _get_str(package_section, "links")
        if links_name:
            entry["links"] = links_name

        lib_section = _get_dict(manifest, "lib")
        if _get_bool(lib_section, "proc-macro"):
            entry["proc_macro"] = True
        entry["build_script"] = build_script_cache.get(key, False)

    # Recompute alias flags in sorted order (only the first entry per package
    # name exposes the bare alias target in defs.bzl).
    seen_norm: set[str] = set()
    for entry in sorted(registry, key=lambda e: (str(e.get("name", "")).lower(), str(e.get("version", "")))):
        norm = normalize(str(entry.get("name", "")))
        entry["alias"] = norm not in seen_norm
        seen_norm.add(norm)

    # Second pass: enrich deps + filter features, then emit each entry.
    entry_strings: list[str] = []
    for entry in registry:
        key = f"{entry['name']}-{entry['version']}"  # type: ignore[index]
        manifest = manifest_cache.get(key, {})
        current_features = set(entry.get("features", []) or [])  # type: ignore[arg-type]
        feats = activated.get(entry["label"], current_features)  # type: ignore[index]
        dep_labels, named_deps, filtered_feats = enrich_dependency_list(
            entry, manifest, feats, lookup, by_label, buck_features, buck_dep_pins
        )
        emit_entry = dict(entry)
        emit_entry["deps"] = dep_labels
        emit_entry["features"] = sorted(filtered_feats)
        emit_entry["named_deps"] = named_deps
        if emit_entry.get("build_script"):
            import_links: dict[str, str] = {}
            for dep_str in dep_labels:
                dep_label = dep_str.lstrip(":")
                dep_entry = by_label.get(dep_label, {})
                dep_links = dep_entry.get("links")
                if dep_links:
                    import_links[str(dep_links)] = dep_label
            if import_links:
                emit_entry["import_links"] = import_links
        entry_strings.append(starlark.format_entry(emit_entry))

    output = starlark.REGISTRY_HEADER + "\n".join(entry_strings) + "\n]\n"
    if not dry_run:
        paths.REGISTRY_BZL.write_text(output, encoding="utf-8")
        print(f"Wrote {paths.REGISTRY_BZL}", file=sys.stderr)
    return output

