#!/usr/bin/env python3
"""Rejects library modules that Cargo would also build as standalone targets.

    python3 .config/ci/test_layout.py [WORKSPACE_ROOT]

Cargo auto-discovers every `tests/*.rs`, `benches/*.rs` and `examples/*.rs`
file as its own crate. A crate whose root is `lib.rs` beside a `tests.rs`
module resolves `mod name;` inside `tests.rs` to `tests/name.rs`: the same
file is then compiled twice, once as the intended unit-test module and once
as a standalone integration test with no parent, so `use super::*` fails.
That broke canonical's `cargo check --all-targets` three times (Backend #154,
#160). Put such modules at `tests/<name>/mod.rs`, which Cargo does not
discover.

Exits 1 and names each offending module; 0 when the layout is clean.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

DISCOVERED_ROOTS = ("tests", "benches", "examples")
# `mod name;` with optional visibility, not `mod name { ... }`.
MOD_DECL = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(r#)?([A-Za-z_][A-Za-z0-9_]*)\s*;", re.MULTILINE)
PATH_ATTR = re.compile(r'#\[path\s*=\s*"([^"]+)"\]\s*$')


def declarations(source: str) -> list[tuple[str, str | None]]:
    """Returns (module name, #[path] value or None) for each out-of-line `mod`."""
    found = []
    lines = source.splitlines()
    for match in MOD_DECL.finditer(source):
        line_index = source.count("\n", 0, match.start())
        path = None
        # A #[path] attribute may sit on the preceding non-empty lines.
        for previous in reversed(lines[max(0, line_index - 3) : line_index]):
            attribute = PATH_ATTR.search(previous.strip())
            if attribute:
                path = attribute.group(1)
                break
            if previous.strip() and not previous.strip().startswith("#["):
                break
        found.append((match.group(2), path))
    return found


def resolve(declaring: Path, name: str, path_attr: str | None) -> list[Path]:
    """Paths rustc would load for `mod name;` declared in `declaring`."""
    if path_attr is not None:
        return [declaring.parent / path_attr]
    if declaring.name in ("mod.rs", "lib.rs", "main.rs"):
        base = declaring.parent
    else:
        base = declaring.parent / declaring.stem
    return [base / f"{name}.rs", base / name / "mod.rs"]


def discovered_targets(package: dict) -> set[Path]:
    """Files Cargo builds as standalone test/bench/example crates."""
    return {
        Path(target["src_path"]).resolve()
        for target in package["targets"]
        if set(target["kind"]) & {"test", "bench", "example"}
    }


def check_package(package_root: Path, targets: set[Path]) -> list[str]:
    problems = []
    skip = {package_root / root for root in DISCOVERED_ROOTS} | {package_root / "target"}
    for source_file in sorted(package_root.rglob("*.rs")):
        if any(parent in skip for parent in source_file.parents):
            continue
        try:
            source = source_file.read_text(errors="replace")
        except OSError:
            continue
        for name, path_attr in declarations(source):
            for candidate in resolve(source_file, name, path_attr):
                candidate = candidate.resolve()
                if candidate in targets and candidate.exists():
                    relative = candidate.relative_to(package_root)
                    problems.append(
                        f"{source_file.relative_to(package_root)} declares `mod {name}`, which loads "
                        f"{relative}; Cargo also builds that file as its own target. "
                        f"Move it to {relative.with_suffix('')}/mod.rs."
                    )
    return problems


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".").resolve()
    metadata = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    members = set(metadata["workspace_members"])
    problems = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        package_root = Path(package["manifest_path"]).parent.resolve()
        for problem in check_package(package_root, discovered_targets(package)):
            problems.append(f"{package['name']}: {problem}")
    for problem in problems:
        print(f"test-layout: {problem}", file=sys.stderr)
    if problems:
        return 1
    print(f"test-layout: {len(members)} workspace packages clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
