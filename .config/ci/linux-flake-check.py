#!/usr/bin/env python3
"""Run the full flake check through one store when CI has its SSH builder."""

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from urllib.parse import quote


def run(args: list[str], *, capture: bool = False) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        args,
        check=False,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        stderr=subprocess.PIPE if capture else None,
    )


def remote_store_from_config() -> str | None:
    """Derive the direct SSH store from the existing task builder setting."""
    config = os.environ.get("NIX_CONFIG", "")
    builder_lines = [
        line.partition("=")[2].strip()
        for line in config.splitlines()
        if line.lstrip().startswith("builders =")
    ]
    if not builder_lines:
        return None

    fields = builder_lines[-1].split()
    if len(fields) < 3 or not fields[0].startswith("ssh-ng://"):
        raise ValueError("NIX_CONFIG has an unsupported SSH builder declaration")
    key_path = Path(fields[2])
    if not key_path.is_file():
        raise ValueError("the configured Nix SSH builder key file is missing")
    return f"{fields[0]}?ssh-key={quote(str(key_path), safe='/')}"


def current_system() -> str:
    result = run(
        ["nix", "eval", "--raw", "--impure", "--expr", "builtins.currentSystem"],
        capture=True,
    )
    if result.returncode:
        sys.stderr.write(result.stderr)
        raise RuntimeError("could not determine the current Nix system")
    system = result.stdout.strip()
    if not re.fullmatch(r"[A-Za-z0-9_-]+", system):
        raise RuntimeError("Nix returned an invalid current system name")
    return system


def current_check_installables(system: str) -> list[str]:
    result = run(
        [
            "nix",
            "eval",
            "--json",
            "--apply",
            "builtins.attrNames",
            f"path:.#checks.{system}",
        ],
        capture=True,
    )
    if result.returncode:
        sys.stderr.write(result.stderr)
        raise RuntimeError("could not enumerate current-system flake checks")
    names = json.loads(result.stdout)
    if not isinstance(names, list) or not names or any(
        not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]+", name)
        for name in names
    ):
        raise RuntimeError("current-system flake check inventory is empty or invalid")
    return [f"path:.#checks.{system}.{name}" for name in names]


def main() -> int:
    try:
        store = remote_store_from_config()
    except (OSError, ValueError) as error:
        print(f"CI problem: {error}", file=sys.stderr)
        return 1

    if store is None:
        # Developer shells and other local callers keep the existing behavior.
        return run(["nix", "flake", "check", "-L", "--keep-going", "path:."]).returncode

    # Evaluate and validate the whole flake without realizing outputs first.
    checked = run(["nix", "flake", "check", "-L", "--keep-going", "--no-build", "path:."])
    if checked.returncode:
        return checked.returncode

    try:
        installables = current_check_installables(current_system())
    except (RuntimeError, json.JSONDecodeError) as error:
        print(f"CI problem: {error}", file=sys.stderr)
        return 1

    # Build every current-system check in one remote-store request. This keeps
    # their derivation graph and realized inputs together across host auto-GC.
    check_names = ", ".join(item.rsplit(".", 1)[1] for item in installables)
    print(f"Linux flake runner: current-system checks: {check_names}")
    print("Linux flake runner: building all checks in one SSH store request")
    return run(
        [
            "nix",
            "build",
            "--no-link",
            "-L",
            "--keep-going",
            "--eval-store",
            "auto",
            "--store",
            store,
            "--max-jobs",
            "1",
            *installables,
        ]
    ).returncode


if __name__ == "__main__":
    sys.exit(main())
