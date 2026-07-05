#!/usr/bin/env python3
"""Fetch SHA-256 for a crates.io release and print a ready-to-paste REGISTRY entry stub.

Usage:
    python3 build/third-party/tools/add-crate.py <name> <version>

Example:
    python3 build/third-party/tools/add-crate.py serde 1.0.200

Prints a dict stub for pasting into build/third-party/registry.bzl REGISTRY list.
You must fill in the "deps" and "features" fields based on the crate's Cargo.toml.
"""

import sys
import hashlib
import urllib.request


def fetch_sha256(name: str, version: str) -> str:
    url = f"https://static.crates.io/crates/{name}/{version}/download"
    print(f"Fetching {url} ...", file=sys.stderr)
    with urllib.request.urlopen(url) as resp:
        data = resp.read()
    return hashlib.sha256(data).hexdigest()


def normalize(name: str) -> str:
    return name.replace("-", "_")


def make_label(name: str, version: str) -> str:
    parts = version.split(".")
    major = parts[0]
    if major == "0" and len(parts) > 1:
        return f"{normalize(name)}-{major}_{parts[1]}"
    return f"{normalize(name)}-{major}"


def main() -> None:
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <name> <version>", file=sys.stderr)
        sys.exit(1)
    name, version = sys.argv[1], sys.argv[2]
    sha256 = fetch_sha256(name, version)
    lbl = make_label(name, version)
    print(f"""    {{
        "name": "{name}",
        "version": "{version}",
        "sha256": "{sha256}",
        "edition": "2021",
        "label": "{lbl}",
        "alias": True,
        "deps": [],        # TODO: fill from crate's Cargo.toml
        "features": [],    # TODO: fill if needed
        "build_script": False,
        "proc_macro": False,
    }},""")


if __name__ == "__main__":
    main()
