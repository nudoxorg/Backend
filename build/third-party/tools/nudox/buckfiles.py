"""Harvest crate/member references from first-party ``workspace/*/BUCK`` files.

These BUCK files call ``deps(crates=[...], members=[...], raw=[...])`` (see
``//build:rust.bzl``).  Rather than shell out to ``buck2 uquery`` (which needs a
working build graph), we ``exec()`` each BUCK file under a stub environment that
captures every ``deps()`` call.  The stubs turn ``rust_crate`` / ``rust_bin`` /
``rust_tests`` / ``load`` / ``native.glob`` / ``select`` into no-ops so the file
runs purely for its data.

Public API
----------
- :func:`harvest` — parse a single BUCK file → ``{"crates", "members", "raw"}``.
- :func:`harvest_all` — union across every ``BUCK`` file under ``workspace/``.
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Any

from . import paths


class _FakeNative:
    """Stand-in for Buck's ``native`` module.

    ``glob`` returns an empty list; any other attribute is a callable no-op so
    exotic ``native.<foo>(...)`` calls inside a BUCK file don't explode.
    """

    @staticmethod
    def glob(*_args: Any, **_kwargs: Any) -> list[Any]:
        return []

    def __getattr__(self, _name: str) -> Any:
        return lambda *a, **k: None


def _make_env(
    referenced: dict[str, set[str]],
    crate_features: dict[str, set[str]],
    collected_dep_pins: dict[str, dict[str, str]],
) -> dict[str, Any]:
    """Build the stub globals dict used to ``exec`` a BUCK file."""

    def _parse_dep_entry(entry: Any) -> None:
        if isinstance(entry, str):
            referenced["crates"].add(entry)
            return
        if not isinstance(entry, dict):
            return
        if entry.get("member") is not None:
            referenced["members"].add(str(entry["member"]))
            return
        if entry.get("target") is not None:
            referenced["raw"].add(str(entry["target"]))
            return
        if entry.get("raw") is not None:
            referenced["raw"].add(str(entry["raw"]))
            return
        if entry.get("pins") is not None:
            for consumer, edges in entry["pins"].items():
                collected_dep_pins.setdefault(str(consumer), {}).update(edges)
            return
        crate_name = entry.get("crate")
        if crate_name is not None:
            if not entry.get("pin_only"):
                referenced["crates"].add(str(crate_name))
            feat_list = entry.get("features")
            if feat_list:
                crate_features.setdefault(str(crate_name), set()).update(feat_list)
            return
        if entry.get("features") is not None:
            for crate_name, feat_list in entry["features"].items():
                crate_features.setdefault(str(crate_name), set()).update(feat_list)
            return

    def _deps(
        spec: Any = None,
        crates: Any = (),
        members: Any = (),
        raw: Any = (),
        features: Any = None,
        dep_pins: Any = None,
    ) -> list[Any]:
        referenced["crates"].update(crates)
        referenced["members"].update(members)
        referenced["raw"].update(raw)
        if features:
            for crate_name, feat_list in features.items():
                crate_features.setdefault(crate_name, set()).update(feat_list)
        pins = dep_pins or {}
        for consumer, edges in pins.items():
            collected_dep_pins.setdefault(consumer, {}).update(edges)
        if spec is not None:
            for entry in spec:
                _parse_dep_entry(entry)
        return []

    def _crate_stub(n: str, features: Any = None, pin_only: bool = False) -> dict[str, Any]:
        entry: dict[str, Any] = {"crate": n}
        if features is not None:
            entry["features"] = list(features)
        if pin_only:
            entry["pin_only"] = True
        return entry

    def _member_stub(m: str) -> dict[str, str]:
        return {"member": m}

    def _target_stub(label: str) -> dict[str, str]:
        return {"target": label}

    def _dep_pins_stub(pins: dict[str, Any]) -> dict[str, Any]:
        return {"pins": pins}

    return {
        "load": lambda *a, **k: None,
        "deps": _deps,
        "crate": _crate_stub,
        "member": _member_stub,
        "target": _target_stub,
        "dep_pins": _dep_pins_stub,
        "workspace": lambda n: n,
        "rust_crate": lambda **k: None,
        "rust_bin": lambda **k: None,
        "rust_library": lambda **k: None,
        "rust_binary": lambda **k: None,
        "rust_tests": lambda **k: None,
        "rust_test": lambda **k: None,
        "select": lambda d: [],
        "glob": lambda *a, **k: [],
        "native": _FakeNative(),
    }


def harvest(buck_path: Path) -> dict[str, set[str]]:
    """Parse a single BUCK file and return its referenced crates/members/raw.

    On any error the file is skipped (a warning is printed to stderr) and the
    partial result recorded so far is returned; a ``"__unparsed__"`` marker set
    carries the file path so callers could later fall back to ``buck2 uquery``.
    """
    referenced: dict[str, set[str]] = {
        "crates": set(),
        "members": set(),
        "raw": set(),
    }
    crate_features: dict[str, set[str]] = {}
    collected_dep_pins: dict[str, dict[str, str]] = {}
    unparsed: set[str] = set()
    env = _make_env(referenced, crate_features, collected_dep_pins)
    try:
        text = Path(buck_path).read_text(encoding="utf-8")
        exec(compile(text, str(buck_path), "exec"), env)  # noqa: S102  # trusted first-party
    except Exception as exc:  # noqa: BLE001 — one exotic file must not kill the sweep
        print(
            f"warning: could not parse {buck_path}: {exc} "
            f"(a buck2 uquery fallback could recover it)",
            file=sys.stderr,
        )
        unparsed.add(str(buck_path))

    result: dict[str, set[str]] = dict(referenced)
    result["unparsed"] = unparsed
    result["features"] = crate_features
    result["dep_pins"] = collected_dep_pins
    return result


def harvest_all(workspace_dir: Path = paths.WORKSPACE_DIR) -> dict[str, set[str]]:
    """Union :func:`harvest` across every ``BUCK`` file under ``workspace/``."""
    merged: dict[str, set[str]] = {
        "crates": set(),
        "members": set(),
        "raw": set(),
        "unparsed": set(),
        "features": {},
        "dep_pins": {},
    }
    for buck_path in sorted(Path(workspace_dir).rglob("BUCK")):
        one = harvest(buck_path)
        for key in ("crates", "members", "raw", "unparsed"):
            merged[key].update(one.get(key, set()))
        for crate_name, feats in one.get("features", {}).items():
            merged["features"].setdefault(crate_name, set()).update(feats)
        for consumer, edges in one.get("dep_pins", {}).items():
            merged["dep_pins"].setdefault(consumer, {}).update(edges)
    return merged


def buck_dep_pins_by_edge(
    harvested: dict[str, set[str]], registry: list[dict[str, Any]]
) -> dict[tuple[str, str], str]:
    """Map ``(consumer_label, dep_alias)`` → pinned registry label.

    Harvested ``dep_pins`` use workspace ``deps(dep_pins={consumer: {dep: label}})``
    aliases; this expands every consumer alias to all matching registry labels.
    """
    from collections import defaultdict

    from .resolve import normalize

    valid_labels = {str(entry["label"]) for entry in registry}
    alias_to_labels: dict[str, list[str]] = defaultdict(list)
    for entry in registry:
        label = str(entry["label"])
        for key in (
            str(entry.get("crate_name") or normalize(str(entry["name"]))),
            normalize(str(entry["name"])),
            str(entry["name"]),
        ):
            if label not in alias_to_labels[key]:
                alias_to_labels[key].append(label)

    result: dict[tuple[str, str], str] = {}
    for consumer_alias, edges in harvested.get("dep_pins", {}).items():
        consumer_labels = (
            alias_to_labels.get(str(consumer_alias))
            or alias_to_labels.get(normalize(str(consumer_alias)))
            or []
        )
        for consumer_label in consumer_labels:
            for dep_alias, pinned_label in edges.items():
                label = str(pinned_label).lstrip(":")
                if label not in valid_labels:
                    print(
                        f"warning: dep_pins {consumer_alias!r} → {dep_alias!r}:"
                        f" {label!r} is not a registry label (skipped)",
                        file=sys.stderr,
                    )
                    continue
                for dep_key in (str(dep_alias), normalize(str(dep_alias))):
                    result[(consumer_label, dep_key)] = label
    return result


def buck_features_by_registry_label(
    harvested: dict[str, set[str]], registry: list[dict[str, Any]]
) -> dict[str, list[str]]:
    """Map ``deps(features={...})`` keys to registry labels.

    * Crate-name keys (``compact_str``) apply to **every** registry label for
      that package — git-sourced crates often depend on a non-alias version.
    * Label keys (``getrandom-0_2``) pin one version so other versions can
      still receive propagated features (e.g. ``getrandom-0_4`` + ``sys_rng``).
    """
    from collections import defaultdict

    from .resolve import normalize

    valid_labels = {str(entry["label"]) for entry in registry}
    alias_to_name: dict[str, str] = {}
    labels_by_name: dict[str, list[str]] = defaultdict(list)
    for entry in registry:
        name = str(entry["name"])
        label = str(entry["label"])
        crate_alias = entry.get("crate_name") or normalize(name)
        alias_to_name[crate_alias] = name
        alias_to_name[normalize(name)] = name
        alias_to_name[name] = name
        labels_by_name[name].append(label)

    result: dict[str, set[str]] = {}
    for alias, feats in harvested.get("features", {}).items():
        key = str(alias)
        if key in valid_labels:
            result.setdefault(key, set()).update(feats)
            continue
        reg_name = alias_to_name.get(key)
        if not reg_name:
            continue
        for label in labels_by_name[reg_name]:
            result.setdefault(label, set()).update(feats)
    return {label: sorted(feats) for label, feats in result.items()}
