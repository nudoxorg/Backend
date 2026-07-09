"""``crates add`` — seed a crate (and its missing transitive deps) into registry.bzl.

Relocated from add-crate.py.  Fetches metadata from crates.io, parses each
crate's Cargo.toml to detect edition / proc-macro / build-script / lib layout,
then inserts the new entries and rewrites registry.bzl (sorted, alias flags
recomputed in final order).
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Any

from .. import buckedit, cratesio, paths, registry
from ..resolve import make_label, normalize


def _concrete(value: Any, fallback: str) -> str:
    """Return a concrete string for a Cargo.toml field.

    Crates that use workspace inheritance store their field values as a table,
    e.g. ``edition = { workspace = true }``.  tomllib parses this as the Python
    dict ``{'workspace': True}``.  When we encounter such a value we must NOT
    forward the dict to starlark.s_str — instead we return ``fallback``, which
    the caller supplies from a more authoritative source (the crates.io index).
    """
    if isinstance(value, dict) and value.get("workspace"):
        return fallback
    if isinstance(value, str):
        return value
    return fallback


def add_arguments(sub: argparse.ArgumentParser) -> None:
    sub.add_argument("name", help="crate name to add")
    sub.add_argument(
        "version", nargs="?", default=None, help="exact version (default: latest)"
    )
    sub.add_argument(
        "--dry-run",
        action="store_true",
        help="print the entries that would be added without modifying the file",
    )
    # Stable CLI surface — accepted and ignored for now.  A later agent will
    # wire `--to <member>` (BUCK patching) via _post_add, and `--features`.
    sub.add_argument(
        "--to",
        default=None,
        help="(not yet implemented) workspace member to add the dependency to",
    )
    sub.add_argument(
        "--features",
        default=None,
        help="(not yet implemented) comma-separated features to enable",
    )


# ── two-pass resolution ───────────────────────────────────────────────────────

def collect_raw(
    name: str,
    version: str | None,
    existing_names: set[str],
    raw_collected: dict[str, dict[str, Any]],
    seen: set[str],
    existing_versions: set[tuple[str, str]] | None = None,
) -> None:
    """Download and parse each crate (and its transitive deps) not already present."""
    if version is None:
        version = cratesio.fetch_latest_version(name)
        print(f"  Resolved {name} → {version}", file=sys.stderr)

    key = f"{name}@{version}"
    if key in seen:
        return
    seen.add(key)

    if existing_versions and (name, version) in existing_versions:
        return
    if not existing_versions and name in existing_names:
        return

    tarball = cratesio.fetch_tarball(name, version)
    sha256 = cratesio.sha256_hex(tarball)
    cargo_toml = cratesio.parse_cargo_toml_from_tarball(tarball, name, version)

    raw_collected[name] = {
        "version": version,
        "sha256": sha256,
        "tarball": tarball,
        "cargo_toml": cargo_toml,
    }

    # Recurse into non-optional, non-git, non-path dependencies
    all_known = existing_names | set(raw_collected)
    all_deps: dict[str, Any] = {}
    for section in ("dependencies", "build-dependencies"):
        all_deps.update(cargo_toml.get(section, {}))

    for dep_name, spec in all_deps.items():
        if isinstance(spec, dict):
            if spec.get("optional") or "git" in spec:
                continue
            if "path" in spec and not spec.get("version"):
                continue
            dep_ver = spec.get("version")
        elif isinstance(spec, str):
            dep_ver = spec
        else:
            dep_ver = None

        if dep_name in all_known:
            continue

        # Strip semver operators to a bare version string, or None for latest
        if dep_ver:
            dep_ver = re.sub(r"^[\^~>=<\s*]+", "", dep_ver).strip().split(",")[0].strip()
            if not re.match(r"^\d+\.\d+\.\d+", dep_ver):
                dep_ver = None

        print(f"  Checking transitive dep: {dep_name}", file=sys.stderr)
        collect_raw(
            dep_name, dep_ver, all_known, raw_collected, seen, existing_versions
        )


def build_entries(
    raw_collected: dict[str, dict[str, Any]],
    existing_registry: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    """Build REGISTRY entry dicts using the full known set of crates."""
    all_name_to_label: dict[str, str] = {
        e["name"]: e["label"] for e in existing_registry
    }
    for name, raw in raw_collected.items():
        all_name_to_label[name] = make_label(name, raw["version"])

    existing_norm = {normalize(e["name"]) for e in existing_registry}
    seen_norm: set[str] = set()
    entries: list[dict[str, Any]] = []

    for name, raw in raw_collected.items():
        version = raw["version"]
        cargo_toml = raw["cargo_toml"]
        tarball = raw["tarball"]

        # Resolve edition: missing field means 2015 (pre-edition crates). Prefer
        # the crates.io index when Cargo.toml uses workspace inheritance.
        raw_edition = cargo_toml.get("package", {}).get("edition")
        if isinstance(raw_edition, dict) and raw_edition.get("workspace"):
            index_edition = cratesio.fetch_version_edition(name, version)
            edition = index_edition if index_edition else "2015"
        elif raw_edition is None:
            index_edition = cratesio.fetch_version_edition(name, version)
            edition = index_edition if index_edition else "2015"
        else:
            edition = _concrete(raw_edition, "2015")
        is_proc = cratesio.detect_proc_macro(cargo_toml)
        is_build = cratesio.detect_build_script(cargo_toml, tarball, name, version)
        lib_root = cratesio.detect_lib_root(cargo_toml)
        crate_name = cratesio.detect_crate_name(cargo_toml, name)
        features = cratesio.detect_default_features(cargo_toml)
        label = make_label(name, version)
        norm = normalize(name)
        alias = norm not in existing_norm and norm not in seen_norm
        seen_norm.add(norm)

        dep_labels: list[str] = []
        all_deps: dict[str, Any] = {}
        for section in ("dependencies", "build-dependencies"):
            all_deps.update(cargo_toml.get(section, {}))
        for dep_name, spec in all_deps.items():
            if isinstance(spec, dict):
                if spec.get("optional") or "git" in spec:
                    continue
                if "path" in spec and not spec.get("version"):
                    continue
            if dep_name in all_name_to_label:
                dep_labels.append(f":{all_name_to_label[dep_name]}")

        entry: dict[str, Any] = {
            "name": name,
            "version": version,
            "sha256": raw["sha256"],
            "edition": edition,
            "label": label,
            "alias": alias,
            "deps": sorted(set(dep_labels)),
            "features": features,
            "build_script": is_build,
            "proc_macro": is_proc,
        }
        if lib_root:
            entry["lib_root"] = lib_root
        if crate_name:
            entry["crate_name"] = crate_name
        entries.append(entry)

    return entries


# ── member -> BUCK path resolution ────────────────────────────────────────────

def _load_members() -> dict[str, str]:
    """Extract the ``_MEMBERS`` dict (member name -> target label) from rust.bzl.

    ``build/rust.bzl`` starts with ``load(...)`` statements that reference other
    ``.bzl`` files; those cannot be ``exec``'d standalone.  Rather than evaluate
    the module, we parse the ``_MEMBERS = { ... }`` block textually, reading each
    ``"member": "//label",`` pair.  Anchor comments are skipped naturally.
    """
    text = paths.RUST_BZL.read_text(encoding="utf-8")
    m = re.search(r"_MEMBERS\s*=\s*\{(?P<body>.*?)\}", text, flags=re.DOTALL)
    if not m:
        return {}
    members: dict[str, str] = {}
    pair_re = re.compile(r'"(?P<name>[^"]+)"\s*:\s*"(?P<label>[^"]+)"')
    for pm in pair_re.finditer(m.group("body")):
        members[pm.group("name")] = pm.group("label")
    return members


def _member_buck_path(target_label: str) -> Path | None:
    """Derive a member's BUCK file path from its ``//workspace/<dir>:...`` label.

    e.g. ``//workspace/server:server-lib`` -> ``<repo>/workspace/server/BUCK``.
    Returns ``None`` if the label doesn't look like a workspace cell target.
    """
    if not target_label.startswith("//"):
        return None
    cell_path = target_label[2:].split(":", 1)[0]  # "workspace/server"
    if not cell_path:
        return None
    return paths.REPO_ROOT / cell_path / "BUCK"


# ── extension point: `--to <member>` BUCK patching ────────────────────────────

def _post_add(args: argparse.Namespace, added_names: list[str]) -> None:
    """Hook run after entries are written.

    If ``--to <member>`` was supplied, insert the primary crate (``args.name``,
    normalised to its Rust extern name) into that member's BUCK
    ``deps(crates=[...])`` list, alphabetically.  Transitive deps that were also
    vendored are intentionally *not* added to the member — only the crate the
    user explicitly asked for.

    Without ``--to`` this is a no-op (registry-only add — the historical
    behaviour).
    """
    member = getattr(args, "to", None)
    if not member:
        return None

    members = _load_members()
    if member not in members:
        print(
            f"⚠️  --to: unknown workspace member '{member}'. "
            f"Known members: {sorted(members)}",
            file=sys.stderr,
        )
        return None

    buck_path = _member_buck_path(members[member])
    if buck_path is None or not buck_path.exists():
        print(
            f"⚠️  --to: could not locate BUCK file for member '{member}' "
            f"(label {members[member]!r}).",
            file=sys.stderr,
        )
        return None

    # The BUCK `crates=[...]` list uses Rust extern names (e.g. `tokio_stream`),
    # so normalise the crate name the same way the registry alias does.
    crate = normalize(args.name)

    text = buck_path.read_text(encoding="utf-8")
    new_text = buckedit.insert_into_deps_list(text, crate)
    if new_text == text:
        print(
            f"'{crate}' already present in {buck_path} (or no crates list found).",
            file=sys.stderr,
        )
        return None

    buck_path.write_text(new_text, encoding="utf-8")
    print(f"Added '{crate}' to {buck_path}", file=sys.stderr)

    # --features is best-effort: features are curated per registry entry during a
    # full `update`/refresh, not wired into an individual member here.  Accept
    # the flag and note it so the request isn't silently lost.
    # TODO: propagate args.features into the registry entry's `features` list on
    # the next refresh; single-entry mutation here is fragile w.r.t. sorting.
    if getattr(args, "features", None):
        print(
            f"note: --features {args.features!r} recorded as a request; "
            f"features are applied on the next `crates update`.",
            file=sys.stderr,
        )

    return None


def _dry_run_post_add(args: argparse.Namespace) -> None:
    """Print what ``_post_add`` WOULD do for ``--to`` without touching files."""
    member = getattr(args, "to", None)
    if not member:
        return
    members = _load_members()
    if member not in members:
        print(
            f"\n[dry-run] --to: unknown member '{member}'. "
            f"Known: {sorted(members)}"
        )
        return
    buck_path = _member_buck_path(members[member])
    crate = normalize(args.name)
    print(
        f"\n[dry-run] would insert '{crate}' into the crates=[...] list of "
        f"{buck_path}"
    )
    if getattr(args, "features", None):
        print(f"[dry-run] would record --features {args.features!r}")


def run(args: argparse.Namespace) -> int:
    name = args.name
    version = args.version
    dry_run = args.dry_run

    print(f"Loading registry ({registry.paths.REGISTRY_BZL})...", file=sys.stderr)
    reg = registry.load_registry()
    print(f"  {len(reg)} existing entries", file=sys.stderr)

    existing_names = {e["name"] for e in reg}
    existing_versions = {(e["name"], e["version"]) for e in reg}
    if version and (name, version) in existing_versions:
        print(f"{name} {version} is already in the registry.", file=sys.stderr)
        return 0
    if name in existing_names and version is None:
        print(
            f"{name} is already in the registry (pass a version to add another).",
            file=sys.stderr,
        )
        return 0

    raw_collected: dict[str, dict[str, Any]] = {}
    seen: set[str] = set()
    collect_raw(
        name, version, existing_names, raw_collected, seen, existing_versions
    )

    if not raw_collected:
        print("Nothing to add.", file=sys.stderr)
        return 0

    new_entries = build_entries(raw_collected, reg)
    sort_key = lambda e: (e["name"].lower(), e["version"])  # noqa: E731
    print(f"\nWill add {len(new_entries)} crate(s):", file=sys.stderr)
    for e in sorted(new_entries, key=sort_key):
        print(f"  {e['name']} {e['version']}", file=sys.stderr)

    if dry_run:
        from .. import starlark

        print("\n--- dry run output ---")
        for e in sorted(new_entries, key=sort_key):
            print(starlark.format_entry(e))
        _dry_run_post_add(args)
        return 0

    all_entries = sorted(reg + new_entries, key=sort_key)

    # Recompute alias flags in final sorted order (exactly as add-crate did).
    seen_norm: set[str] = set()
    for e in all_entries:
        norm = normalize(e["name"])
        e["alias"] = norm not in seen_norm
        seen_norm.add(norm)

    registry.write_registry(all_entries)
    print(
        f"\nWrote {len(all_entries)} entries to {registry.paths.REGISTRY_BZL}",
        file=sys.stderr,
    )

    _post_add(args, list(raw_collected.keys()))
    return 0
