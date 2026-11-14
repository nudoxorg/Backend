#!/usr/bin/env python3
"""Materialize the exact registry crates and apply the experimental patches."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile

here = Path(__file__).resolve().parent
cargo_home = Path(os.environ.get("CARGO_HOME", str(Path.home() / ".cargo")))
packages = [
    ("stylo", "0.23.0", "84fe6be1e185e318b93be8f1e025667c196584a9bc9d5b7aca34bba4a07fb488"),
    ("selectors", "0.42.0", "284ac2879477575f13b22201f7e98aa33aacc765b790536808cf580fd79339f1"),
]
for name, version, expected in packages:
    matches = list((cargo_home / "registry" / "cache").glob(f"*/{name}-{version}.crate"))
    if len(matches) != 1:
        raise SystemExit(f"expected exactly one cached {name}-{version}.crate, found {len(matches)}")
    archive = matches[0]
    if hashlib.sha256(archive.read_bytes()).hexdigest() != expected:
        raise SystemExit(f"registry checksum mismatch: {archive}")
    vendor = here / "vendor"
    vendor.mkdir(exist_ok=True)
    destination = vendor / name
    if destination.exists():
        raise SystemExit(f"refusing to replace existing {destination}")
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar:
            parts = Path(member.name).parts
            if not parts or parts[0] != f"{name}-{version}" or any(part in ("..", ".") for part in parts):
                raise SystemExit(f"invalid archive path: {member.name}")
            target = vendor.joinpath(*parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                target.parent.mkdir(parents=True, exist_ok=True)
                with tar.extractfile(member) as source, target.open("wb") as output:
                    shutil.copyfileobj(source, output)
            else:
                raise SystemExit(f"unsupported archive entry: {member.name}")
    shutil.move(str(vendor / f"{name}-{version}"), str(destination))
    with (here / f"{name}.patch").open("rb") as patch:
        subprocess.run(["patch", "-p1"], cwd=destination, stdin=patch, check=True)
