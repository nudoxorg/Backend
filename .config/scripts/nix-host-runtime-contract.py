#!/usr/bin/env python3
"""Check Nix's compiler runtime environment against LocalHost's closed names."""

from __future__ import annotations

import re
import sys
from pathlib import Path


def fail(message: str) -> None:
    print(f"local-host runtime contract: {message}", file=sys.stderr)
    raise SystemExit(1)


def read(path: str) -> str:
    try:
        return Path(path).read_text(encoding="utf-8")
    except OSError as error:
        fail(f"cannot read {path}: {error}")


def main() -> None:
    if len(sys.argv) != 10:
        fail(
            "usage: nix-host-runtime-contract.py HOST CORPUS_ENV SHELLS "
            "TS_FRONTEND GO_FRONTEND PYTHON_FRONTEND CSHARP_FRONTEND "
            "FLEET_RS FLEET_SH"
        )

    (
        _,
        host_path,
        corpus_env_path,
        shells_path,
        ts_frontend_path,
        go_frontend_path,
        python_frontend_path,
        csharp_frontend_path,
        fleet_rs_path,
        fleet_sh_path,
    ) = sys.argv
    host = read(host_path)
    corpus_env = read(corpus_env_path)
    shells = read(shells_path)

    enum = re.search(r"pub enum LocalHostVariable\s*\{(.*?)\n\}", host, re.S)
    mapping = re.search(
        r"const fn variable_name\(variable: LocalHostVariable\).*?match variable \{(.*?)\n    \}",
        host,
        re.S,
    )
    if enum is None or mapping is None:
        fail("LocalHostVariable enum or variable_name mapping was not found")
    variants = set(re.findall(r"^\s{4}([A-Z][A-Za-z0-9_]*),\s*$", enum.group(1), re.M))
    arms = re.findall(
        r'LocalHostVariable::([A-Za-z0-9_]+)\s*=>\s*"([A-Z][A-Z0-9_]*)"',
        mapping.group(1),
    )
    mapped_variants = [variant for variant, _ in arms]
    names = [name for _, name in arms]
    if len(mapped_variants) != len(set(mapped_variants)):
        fail("variable_name maps a LocalHostVariable variant more than once")
    if len(names) != len(set(names)):
        fail("variable_name maps two variants to the same process variable")
    if set(mapped_variants) != variants:
        fail(
            "closed mapping drift: "
            f"unmapped={sorted(variants - set(mapped_variants))}, "
            f"unknown={sorted(set(mapped_variants) - variants)}"
        )
    host_names = set(names)

    # These values belong to the application or deployment profile, rather
    # than to the pinned Nix toolchain. Package roots are selected per owner.
    supplied_by_process_or_profile = {
        "HOME",
        "XDG_DATA_HOME",
        "LOCALAPPDATA",
        "NUDOX_DATA_ROOT",
        "NUDOX_CARGO_ROOT",
        "NUDOX_NPM_ROOT",
        "NUDOX_PYPI_ROOT",
        "NUDOX_MAVEN_ROOT",
        "NUDOX_NUGET_ROOT",
        "NUDOX_GENERIC_ROOT",
    }
    if not supplied_by_process_or_profile <= host_names:
        fail(
            "process/profile exception contains names outside the closed host set: "
            f"{sorted(supplied_by_process_or_profile - host_names)}"
        )

    declared_nix = set(re.findall(r"^\s*([A-Z][A-Z0-9_]*)\s*=", corpus_env, re.M))
    declared_nix.update(re.findall(r"\bexport\s+([A-Z][A-Z0-9_]*)=", shells))
    observed_host_names = declared_nix & host_names
    required_host_names = host_names - supplied_by_process_or_profile
    if observed_host_names != required_host_names:
        fail(
            "Nix/LocalHost runtime set drift: "
            f"missing={sorted(required_host_names - observed_host_names)}, "
            f"unexpected={sorted(observed_host_names - required_host_names)}"
        )

    # Keep these adapter aliases only while the standalone frontend paths
    # actually read them. LocalHost gets the exact typed names above.
    aliases = {
        "NUDOX_TYPESCRIPT_CHECKER_BIN": read(ts_frontend_path),
        "NUDOX_GO_ORACLE_BIN": read(go_frontend_path),
        "NUDOX_PYREFLY_BIN": read(python_frontend_path),
        "NUDOX_CSHARP_DOTNET": read(csharp_frontend_path),
    }
    nix_names = declared_nix
    for name, consumer in aliases.items():
        if name not in nix_names or name not in consumer:
            fail(f"frontend adapter {name} lacks a live Nix declaration and source consumer")

    unclassified_nudox = {
        name
        for name in nix_names
        if name.startswith("NUDOX_")
        and name not in host_names
        and not name.endswith("_CORPUS_DIR")
        and name not in aliases
    }
    if unclassified_nudox:
        fail(f"Nix exports unclassified NUDOX variables: {sorted(unclassified_nudox)}")
    if "NUDOX_CLANG_DRIVER" in nix_names:
        fail("Nix still exports the unused NUDOX_CLANG_DRIVER compatibility name")

    fleet_rs = read(fleet_rs_path)
    fleet_sh = read(fleet_sh_path)
    array = re.search(r"pub const AUTHORITY_VARIABLES: \[&str; \d+\] = \[(.*?)\];", fleet_rs, re.S)
    if array is None:
        fail("fleet authority variable list was not found")
    rust_fleet_names = set(re.findall(r'"([A-Z][A-Z0-9_]*)"', array.group(1)))
    shell_fleet_names = set(re.findall(r"^check_authority\s+([A-Z][A-Z0-9_]*)\s+", fleet_sh, re.M))
    if rust_fleet_names != shell_fleet_names:
        fail(
            "fleet executable-admission names drift: "
            f"Rust-only={sorted(rust_fleet_names - shell_fleet_names)}, "
            f"shell-only={sorted(shell_fleet_names - rust_fleet_names)}"
        )
    if "NUDOX_CLANG" not in rust_fleet_names or "NUDOX_CLANG_DRIVER" in rust_fleet_names:
        fail("fleet must admit the Host-recognized NUDOX_CLANG executable, not its stale alias")
    for required in ("NUDOX_CARGO", "NUDOX_TYPESCRIPT_REPORT_PROGRAM", "NUDOX_GO_ORACLE", "NUDOX_ROSLYN_HELPER"):
        if required not in rust_fleet_names:
            fail(f"fleet does not verify Host runtime authority {required}")

    print(
        "local-host runtime contract: "
        f"{len(host_names)} closed names; {len(required_host_names)} pinned or shell-resolved; "
        f"{len(supplied_by_process_or_profile)} process/profile-owned; "
        f"{len(aliases)} live frontend compatibility adapters"
    )


if __name__ == "__main__":
    main()
