#!/usr/bin/env python3
# Gives unchanged sources back the mtimes they had when the cached build
# output was made, so Cargo reuses it. Usage: restore-mtimes.py MANIFEST
#
# Cargo rebuilds a workspace crate when any of its source files is newer
# than its last build. A fresh clone stamps every file "now", which makes a
# persistent build directory worthless. A file whose git blob matches the
# manifest gets its recorded mtime back; a changed or new file keeps "now"
# and is rebuilt. The manifest is then rewritten from what is on disk, so a
# build that failed part-way is still judged correctly on the next run.
#
# Directories matter too: a build script that says
# `cargo::rerun-if-changed=<dir>` is judged by the newest mtime inside that
# directory, the directories included. A clone stamps every directory "now",
# so vendored pyrefly's `rerun-if-changed=test_laziness` reran on every T1
# run and recompiled backend-engine and everything above it (250-330 s,
# Concourse 452816). Each tracked directory whose set of tracked entries is
# unchanged gets the newest mtime of its contents; one whose entries changed
# (a file added, removed or renamed) keeps "now".
import hashlib
import json
import os
import subprocess
import sys

DIRECTORIES = "\0directories"


def directory_listings(paths):
    """Maps every ancestor directory of `paths` to a digest of its entries."""
    children = {}
    for path in paths:
        parts = path.split("/")
        for depth in range(len(parts) - 1):
            parent = "/".join(parts[:depth]) or "."
            children.setdefault(parent, set()).add(parts[depth])
        children.setdefault("/".join(parts[:-1]) or ".", set()).add(parts[-1])
    return {
        directory: hashlib.sha256("\0".join(sorted(names)).encode()).hexdigest()
        for directory, names in children.items()
    }


def main(manifest_path):
    try:
        with open(manifest_path) as manifest_file:
            manifest = json.load(manifest_file)
    except (FileNotFoundError, ValueError):
        manifest = {}
    recorded_directories = manifest.pop(DIRECTORIES, {})

    listing = subprocess.run(
        ["git", "ls-files", "--stage", "-z"], check=True, capture_output=True
    ).stdout
    current = {}
    restored = 0
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        meta, raw_path = entry.split(b"\t", 1)
        mode, blob, _stage = meta.split()
        path = os.fsdecode(raw_path)
        # Regular files only: symlinks and submodules carry no build mtime.
        if mode not in (b"100644", b"100755") or not os.path.isfile(path):
            continue
        blob = blob.decode()
        recorded = manifest.get(path)
        if recorded is not None and recorded["blob"] == blob:
            os.utime(path, ns=(recorded["mtime"], recorded["mtime"]))
            mtime = recorded["mtime"]
            restored += 1
        else:
            mtime = os.stat(path).st_mtime_ns
        current[path] = {"blob": blob, "mtime": mtime}

    # Newest content mtime per directory. Setting a directory's mtime never
    # changes its parent's, so the order of utime calls does not matter.
    newest = {}
    for path, record in current.items():
        parts = path.split("/")
        for depth in range(len(parts)):
            directory = "/".join(parts[:depth]) or "."
            if record["mtime"] > newest.get(directory, 0):
                newest[directory] = record["mtime"]
    listings = directory_listings(current)
    directories_restored = 0
    for directory, digest in listings.items():
        if recorded_directories.get(directory) != digest or not os.path.isdir(directory):
            continue
        os.utime(directory, ns=(newest[directory], newest[directory]))
        directories_restored += 1

    current[DIRECTORIES] = listings
    temporary = manifest_path + ".tmp"
    with open(temporary, "w") as manifest_file:
        json.dump(current, manifest_file)
    os.replace(temporary, manifest_path)
    print(
        f"source mtimes: {restored} of {len(current) - 1} files and "
        f"{directories_restored} of {len(listings)} directories restored from the cache manifest"
    )


if __name__ == "__main__":
    main(sys.argv[1])
