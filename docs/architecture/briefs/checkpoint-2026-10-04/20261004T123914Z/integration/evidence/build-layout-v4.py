#!/usr/bin/env python3
"""Create isolated empty Cargo build roots while reusing only cache symlinks."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import shutil
import stat
import sys
import time
from typing import List

COMMIT = "9a896bf197aed9028e9dbf14f0532eb00daad077"
TREE = "19146a7b96b3dda49d856ba777dc3794a7e20695"
LOCK_SHA = "5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f"


class LayoutError(RuntimeError):
    pass


def check_dir(path: pathlib.Path, private: bool = False) -> os.stat_result:
    if path.is_symlink() or not path.is_dir():
        raise LayoutError("directory missing or symlinked: " + str(path))
    info = path.lstat()
    if info.st_uid != os.getuid():
        raise LayoutError("directory owner mismatch: " + str(path))
    mode = stat.S_IMODE(info.st_mode)
    if private and mode != 0o700:
        raise LayoutError("private directory must be mode 0700: " + str(path))
    if not private and mode & 0o022:
        raise LayoutError("directory is group/world writable: " + str(path))
    return info


def descendants_of(path: pathlib.Path, root: pathlib.Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def cache_links(source_home: pathlib.Path, shared_cache: pathlib.Path) -> List[dict]:
    check_dir(source_home, private=True)
    # Cached source/build objects may be readable by other users; only the
    # owner may mutate the approved cache. Per-attempt roots remain 0700.
    check_dir(shared_cache, private=False)
    links = []
    for item in sorted(source_home.iterdir(), key=lambda p: p.name):
        info = item.lstat()
        if not stat.S_ISLNK(info.st_mode):
            # Do not copy config, credentials, private files, or non-cache state.
            continue
        raw_target = os.readlink(str(item))
        if os.path.isabs(raw_target):
            candidate = pathlib.Path(raw_target)
        else:
            candidate = item.parent / raw_target
        normalized = pathlib.Path(os.path.normpath(str(candidate)))
        if not descendants_of(normalized, shared_cache):
            raise LayoutError("Cargo home symlink escapes the approved shared cache: " + item.name)
        if not normalized.exists() and not normalized.is_symlink():
            raise LayoutError("Cargo cache symlink target is missing: " + item.name)
        resolved = normalized.resolve(strict=True)
        cache_root = shared_cache.resolve(strict=True)
        if not descendants_of(resolved, cache_root):
            raise LayoutError("resolved Cargo cache symlink escapes the approved shared cache: " + item.name)
        links.append({"name": item.name, "original_target": raw_target,
                      "target": str(resolved), "resolved": str(resolved)})
    if not links:
        raise LayoutError("no approved Cargo cache symlinks found in the existing Cargo home")
    return links


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-release", required=True, type=pathlib.Path)
    parser.add_argument("--base-cargo-home", required=True, type=pathlib.Path)
    parser.add_argument("--shared-cache", required=True, type=pathlib.Path)
    parser.add_argument("--receipt-parent", required=True, type=pathlib.Path)
    parser.add_argument("--run-cargo", required=True, type=pathlib.Path)
    parser.add_argument("--execute-stage", required=True, type=pathlib.Path)
    parser.add_argument("--run-sequence", required=True, type=pathlib.Path)
    args = parser.parse_args()

    source_root = args.source_release / "source"
    check_dir(args.source_release, private=True)
    check_dir(source_root, private=False)
    check_dir(args.receipt_parent, private=False)
    source_lock = source_root / "Cargo.lock"
    if source_lock.is_symlink() or not source_lock.is_file():
        raise LayoutError("candidate Cargo.lock is missing or symlinked")
    import hashlib
    digest = hashlib.sha256(source_lock.read_bytes()).hexdigest()
    if digest != LOCK_SHA:
        raise LayoutError("candidate lock hash mismatch")

    links = cache_links(args.base_cargo_home, args.shared_cache)
    receipt = args.receipt_parent / ("validation-" + COMMIT)
    if receipt.exists() or receipt.is_symlink():
        raise LayoutError("refusing existing build receipt path: " + str(receipt))
    receipt.mkdir(mode=0o700)
    check_dir(receipt, private=True)
    cargo_home = receipt / "cargo-home"
    target_dir = receipt / "cargo-target"
    build_dir = receipt / "cargo-build-graph"
    for path in (cargo_home, target_dir, build_dir):
        path.mkdir(mode=0o700)
        check_dir(path, private=True)
    for link in links:
        target = cargo_home / link["name"]
        os.symlink(link["resolved"], str(target))
        observed = target.resolve(strict=True)
        if observed != pathlib.Path(link["resolved"]):
            raise LayoutError("new Cargo home symlink does not resolve to the approved cache object: " + target.name)
        if not descendants_of(observed, args.shared_cache.resolve(strict=True)):
            raise LayoutError("new Cargo home symlink escaped the approved shared cache: " + target.name)
    wrapper_hashes = {}
    for wrapper in (args.run_cargo, args.execute_stage, args.run_sequence):
        if wrapper.is_symlink() or not wrapper.is_file():
            raise LayoutError("reviewed wrapper is missing or not a regular file: " + str(wrapper))
        target = receipt / wrapper.name
        if target.exists() or target.is_symlink():
            raise LayoutError("refusing existing wrapper destination: " + str(target))
        shutil.copyfile(str(wrapper), str(target), follow_symlinks=False)
        os.chmod(target, 0o600)
        wrapper_hashes[wrapper.name] = hashlib.sha256(target.read_bytes()).hexdigest()
    result = {
        "status": "isolated_empty_build_roots_prepared",
        "source_commit": COMMIT,
        "source_tree": TREE,
        "cargo_lock_sha256": digest,
        "source_release": str(args.source_release),
        "receipt": str(receipt),
        "cargo_home": str(cargo_home),
        "target_dir": str(target_dir),
        "build_graph_dir": str(build_dir),
        "shared_cache": str(args.shared_cache),
        "reused_symlinks_only": links,
        "reviewed_wrapper_sha256": wrapper_hashes,
        "copied_cargo_credentials_or_config": False,
        "old_source_target_or_build_graph_reused": False,
        "prepared_utc_epoch": int(time.time()),
        "no_cargo_invoked": True,
    }
    result_file = receipt / "layout-result.json"
    with result_file.open("x", encoding="utf-8") as stream:
        json.dump(result, stream, sort_keys=True, indent=2)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(result_file, 0o600)
    print(json.dumps({"status": result["status"], "receipt": str(receipt),
                      "cargo_home": str(cargo_home), "target_dir": str(target_dir),
                      "build_graph_dir": str(build_dir), "cache_links": len(links)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (LayoutError, OSError) as exc:
        print("LAYOUT_FAILED: " + str(exc), file=sys.stderr)
        raise SystemExit(2)
