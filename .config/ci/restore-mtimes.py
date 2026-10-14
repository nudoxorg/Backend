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
# Concourse 452816). A directory is restored only when its actual direct
# entries and entry kinds match Git's tracked tree; unknown entries keep it
# and its ancestors at "now" so Cargo can see real directory inputs. Verified
# directory timestamps are retained as well: resetting a removed input's
# directory to its remaining files' older times could hide an interrupted build.
import hashlib
import json
import os
import stat
import subprocess
import sys

DIRECTORIES = "\0directories"


def tracked_kind(mode):
    """Returns the worktree entry kind represented by a Git index mode."""
    if mode in (b"100644", b"100755"):
        return "file"
    if mode == b"120000":
        return "symlink"
    if mode == b"160000":
        return "gitlink"
    return "other"


def actual_kind(mode):
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISDIR(mode):
        return "directory"
    if stat.S_ISLNK(mode):
        return "symlink"
    return "other"


def directory_listings(entries):
    """Maps tracked paths to expected child kinds and content fingerprints."""
    children = {}
    for path, entry in entries.items():
        parts = path.split("/")
        for depth in range(len(parts) - 1):
            parent = "/".join(parts[:depth]) or "."
            children.setdefault(parent, {})[parts[depth]] = ("directory", None)
        parent = "/".join(parts[:-1]) or "."
        children.setdefault(parent, {})[parts[-1]] = entry
    # Include entry kinds and Git object IDs as well as names. A path-class or
    # tracked-content change is a directory input change even if its name is
    # unchanged. This also makes old name-only manifests fail closed once.
    digests = {}
    for directory, entries in children.items():
        encoded = json.dumps(
            sorted((name, kind, blob) for name, (kind, blob) in entries.items()),
            separators=(",", ":"),
        )
        digests[directory] = hashlib.sha256(encoded.encode()).hexdigest()
    expected_kinds = {
        directory: {
            name: "directory" if kind == "gitlink" else kind
            for name, (kind, _blob) in entries.items()
        }
        for directory, entries in children.items()
    }
    opaque_directories = {
        directory
        for directory, entries in children.items()
        if any(kind in ("gitlink", "other") for kind, _blob in entries.values())
    }
    return expected_kinds, digests, opaque_directories


def directory_matches_tracked_entries(directory, expected):
    """Checks one directory without following links or walking its children."""
    try:
        if not stat.S_ISDIR(os.lstat(directory).st_mode):
            return False
        actual = {}
        with os.scandir(directory) as entries:
            for entry in entries:
                # Git metadata is never a Cargo source entry. Worktrees may
                # store it as a file and ordinary checkouts as a directory.
                if directory == "." and entry.name == ".git":
                    continue
                actual[entry.name] = actual_kind(
                    entry.stat(follow_symlinks=False).st_mode
                )
    except OSError:
        return False
    return actual == expected


def tracked_worktree_is_clean():
    """Only reuse timestamps when the index also describes worktree bytes."""
    result = subprocess.run(["git", "diff", "--quiet", "HEAD"], check=False)
    if result.returncode == 1:
        return False
    result.check_returncode()
    return True


def main(manifest_path):
    if not tracked_worktree_is_clean():
        print("source mtimes: skipped because tracked checkout differs from HEAD")
        return

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
    tracked_entries = {}
    regular_files = {}
    for entry in listing.split(b"\0"):
        if not entry:
            continue
        meta, raw_path = entry.split(b"\t", 1)
        mode, _blob, stage = meta.split()
        path = os.fsdecode(raw_path)
        if path in tracked_entries:
            # An unmerged index has no single proven source entry.
            tracked_entries[path] = ("other", None)
            regular_files.pop(path, None)
            continue
        blob = _blob.decode() if stage == b"0" else None
        tracked_entries[path] = (tracked_kind(mode), blob)
        if stage != b"0" or mode not in (b"100644", b"100755"):
            continue
        try:
            if stat.S_ISREG(os.lstat(path).st_mode):
                regular_files[path] = blob
        except OSError:
            pass

    restored = 0
    for path, blob in regular_files.items():
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
    expected_entries, listings, opaque_directories = directory_listings(tracked_entries)
    # A tracked-only hash cannot account for ignored or untracked children.
    # Check the actual direct entry set (and each child's path class) only in
    # directories represented by tracked paths. An incomplete directory and
    # its ancestors cannot be restored or written into the next manifest:
    # otherwise removing the unknown child on a later run could make its
    # removal look like a previously verified tree.
    incomplete = set()
    for directory, expected in expected_entries.items():
        # A gitlink names a submodule commit, but the parent checkout does
        # not prove the submodule worktree's contents. Keep its containing
        # directory and ancestors opaque rather than walking that tree.
        if directory not in opaque_directories and directory_matches_tracked_entries(directory, expected):
            continue
        ancestor = directory
        while ancestor in listings and ancestor not in incomplete:
            incomplete.add(ancestor)
            ancestor = os.path.dirname(ancestor) or "."

    # A complete current tree whose signature differs from the prior manifest
    # is recorded for a later run, but never restored on this run. Propagate
    # that miss so an ancestor cannot hide a changed nested directory.
    not_reusable = set(incomplete)
    for directory, digest in listings.items():
        recorded = recorded_directories.get(directory)
        valid = (
            isinstance(recorded, dict)
            and recorded.get("digest") == digest
            and type(recorded.get("mtime")) is int
            and recorded["mtime"] >= 0
        )
        if directory in incomplete or valid:
            continue
        ancestor = directory
        while ancestor in listings and ancestor not in not_reusable:
            not_reusable.add(ancestor)
            ancestor = os.path.dirname(ancestor) or "."

    directories_restored = 0
    for directory, digest in listings.items():
        if (
            directory in not_reusable
            or directory not in newest
        ):
            continue
        mtime = max(recorded_directories[directory]["mtime"], newest[directory])
        os.utime(directory, ns=(mtime, mtime))
        directories_restored += 1

    # Record the actual directory invalidation time, not just remaining files'
    # times. The manifest precedes Cargo: a cancelled or failed build must keep
    # a removal newer than its old cached output on every later retry. A
    # completed build naturally moves its output beyond this stable timestamp.
    current[DIRECTORIES] = {
        directory: {
            "digest": digest,
            "mtime": max(os.lstat(directory).st_mtime_ns, newest.get(directory, 0)),
        }
        for directory, digest in listings.items()
        if directory not in incomplete
    }
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
