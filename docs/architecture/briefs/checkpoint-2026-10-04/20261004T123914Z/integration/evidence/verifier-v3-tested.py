#!/usr/bin/env python3
"""Verify an expanded Nudox source tree against the pinned 9a manifest."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import sys
from collections.abc import Mapping
from typing import Optional

BASE_MANIFEST_SHA = "ca834ddcdf16990d59fd040476fecfa3591b20161f3ae527dfd6b7737baea7a2"
FINAL_MANIFEST_SHA = "534314ceba4c04932855a22c3cca406f03ba46eeebd189ff38fbcecab44b23cd"
BASE_COMMIT = "13d9919ae6a9fc4d662c5ffb322462c6e3dfb25d"
BASE_TREE = "e8a0ba54bdfae0698aca136b9400d9a1c8a89817"
FINAL_COMMIT = "9a896bf197aed9028e9dbf14f0532eb00daad077"
FINAL_TREE = "19146a7b96b3dda49d856ba777dc3794a7e20695"
GITLINK_COMMIT = "052ad81a7dbb6efed34ea482ae6c7e2e6f6682e0"
GITLINK_TREE = "8f51914241b19bb815e49fe1c4ba56fc46da6ea2"
LOCK_SHA = "5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f"
PARENT_ENTRIES = 5125
GITLINK_ENTRIES = 4108
TOTAL_ENTRIES = PARENT_ENTRIES + GITLINK_ENTRIES
FIELDS = ("type", "mode", "bytes", "sha256")
EXPECTED_CHANGED_PATHS = ('crates/library/benches/package_graph.rs', 'crates/library/lib.rs', 'crates/library/package_graph.rs', 'crates/library/package_graph_page.rs', 'crates/library/semantic_shape.rs', 'crates/library/wire/reply_semantic_shape.rs', 'crates/local-service/src/builtin.rs', 'crates/local-service/src/builtin/commands/adapter.rs', 'crates/local-service/src/builtin/local_manifest_residence.rs', 'crates/local-service/src/builtin/product_state.rs', 'crates/local-service/src/builtin/registry.rs', 'crates/local-service/src/process.rs', 'extensions/turso/src/tests.rs')


class VerificationError(RuntimeError):
    pass


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def safe_relative(raw: object) -> pathlib.PurePosixPath:
    if not isinstance(raw, str) or not raw or "\x00" in raw or "\\" in raw:
        raise VerificationError(f"invalid manifest path: {raw!r}")
    rel = pathlib.PurePosixPath(raw)
    if rel.is_absolute() or any(part in ("", ".", "..") for part in rel.parts):
        raise VerificationError(f"unsafe manifest path: {raw!r}")
    if rel.as_posix() != raw:
        raise VerificationError(f"non-canonical manifest path: {raw!r}")
    return rel


def load_manifest(path: pathlib.Path, expected_sha: str, kind: str) -> dict:
    if path.is_symlink() or not path.is_file():
        raise VerificationError(f"manifest is missing or is a symlink: {path}")
    observed_sha = sha256_file(path)
    if observed_sha != expected_sha:
        raise VerificationError(f"manifest SHA-256 mismatch: {path}")
    try:
        document = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise VerificationError(f"cannot decode manifest {path}: {exc}") from exc
    source = document.get("source")
    gitlink = document.get("gitlink")
    if not isinstance(source, dict) or not isinstance(gitlink, dict):
        raise VerificationError("manifest has no source/gitlink objects")
    if source.get("cargo_lock_sha256") != LOCK_SHA:
        raise VerificationError("manifest Cargo.lock identity mismatch")
    if source.get("tracked_blob_entries") != PARENT_ENTRIES or len(source.get("files", [])) != PARENT_ENTRIES:
        raise VerificationError("parent source entry count mismatch")
    if gitlink.get("files_count") != GITLINK_ENTRIES or len(gitlink.get("files", [])) != GITLINK_ENTRIES:
        raise VerificationError("gitlink entry count mismatch")
    if gitlink.get("commit") != GITLINK_COMMIT or gitlink.get("tree") != GITLINK_TREE:
        raise VerificationError("Turso gitlink identity mismatch")
    if gitlink.get("path") != "server/index/turso":
        raise VerificationError("unexpected gitlink root")
    if kind == "base":
        if source.get("commit") != BASE_COMMIT or source.get("tree") != BASE_TREE:
            raise VerificationError("base commit/tree mismatch")
    elif kind == "final":
        if source.get("commit") != FINAL_COMMIT or source.get("tree") != FINAL_TREE:
            raise VerificationError("candidate commit/tree mismatch")
        transport = document.get("transport")
        if not isinstance(transport, dict):
            raise VerificationError("candidate transport receipt missing")
        if transport.get("base_commit") != BASE_COMMIT or transport.get("base_tree") != BASE_TREE:
            raise VerificationError("candidate base identity mismatch")
        if transport.get("base_manifest_sha256") != BASE_MANIFEST_SHA:
            raise VerificationError("candidate base manifest mismatch")
        if transport.get("delta_archive_sha256") != "78c35b5b7d1c992bdb507d8d4dafcbadf537eaa1cbc7c18f938a873d4e85cdba":
            raise VerificationError("candidate delta identity mismatch")
        changed_paths = transport.get("changed_paths")
        if not isinstance(changed_paths, list) or len(changed_paths) != len(EXPECTED_CHANGED_PATHS):
            raise VerificationError("candidate changed-path list/count mismatch")
        canonical_paths = tuple(safe_relative(path).as_posix() for path in changed_paths)
        if len(set(canonical_paths)) != len(canonical_paths):
            raise VerificationError("candidate changed-path list contains duplicates")
        if tuple(sorted(canonical_paths)) != EXPECTED_CHANGED_PATHS:
            raise VerificationError("candidate changed-path set differs from the pinned delta")
    else:
        raise VerificationError(f"unknown manifest kind: {kind}")
    return document


def entries_by_path(document: Mapping) -> dict[str, dict]:
    entries: dict[str, dict] = {}
    for section in ("source", "gitlink"):
        for entry in document[section]["files"]:
            raw_path = entry.get("path")
            safe_relative(raw_path)
            if raw_path in entries:
                raise VerificationError(f"duplicate manifest path: {raw_path}")
            if entry.get("type") not in ("file", "symlink"):
                raise VerificationError(f"unsupported manifest entry type: {raw_path}")
            try:
                mode = int(entry["mode"], 8) if isinstance(entry["mode"], str) else int(entry["mode"])
                size = int(entry["bytes"])
                digest = str(entry["sha256"])
            except (KeyError, TypeError, ValueError) as exc:
                raise VerificationError(f"malformed manifest entry: {raw_path}") from exc
            if mode < 0 or size < 0 or len(digest) != 64:
                raise VerificationError(f"invalid mode/size/hash: {raw_path}")
            entries[raw_path] = entry
    if len(entries) != TOTAL_ENTRIES:
        raise VerificationError(f"expected {TOTAL_ENTRIES} total entries, got {len(entries)}")
    return entries


def inspect_entry(path: pathlib.Path) -> tuple[str, str, int, str, os.stat_result]:
    item_stat = path.lstat()
    if stat.S_ISREG(item_stat.st_mode):
        kind = "file"
        digest = sha256_file(path)
        size = item_stat.st_size
    elif stat.S_ISLNK(item_stat.st_mode):
        kind = "symlink"
        link_bytes = os.fsencode(os.readlink(path))
        digest = sha256_bytes(link_bytes)
        size = len(link_bytes)
    else:
        raise VerificationError(f"unsupported filesystem object: {path}")
    return kind, oct(stat.S_IMODE(item_stat.st_mode)), size, digest, item_stat


def walk_payload(root: pathlib.Path) -> dict[str, tuple[str, str, int, str, os.stat_result]]:
    if root.is_symlink() or not root.is_dir():
        raise VerificationError(f"source root is missing or is a symlink: {root}")
    if (root / ".git").exists() or (root / ".git").is_symlink():
        raise VerificationError("expanded source must not contain .git metadata")
    found = {}
    stack = [(root, pathlib.PurePosixPath())]
    while stack:
        directory, prefix = stack.pop()
        with os.scandir(directory) as scan:
            items = sorted(scan, key=lambda item: item.name)
        for item in items:
            child = pathlib.Path(item.path)
            rel = (prefix / item.name).as_posix()
            safe_relative(rel)
            child_stat = item.stat(follow_symlinks=False)
            if stat.S_ISDIR(child_stat.st_mode):
                stack.append((child, pathlib.PurePosixPath(rel)))
            else:
                found[rel] = inspect_entry(child)
    return found


def verify_tree(root: pathlib.Path, document: Mapping, base_for_inode_check: Optional[pathlib.Path] = None) -> dict:
    expected = entries_by_path(document)
    found = walk_payload(root)
    expected_paths = set(expected)
    actual_paths = set(found)
    missing = sorted(expected_paths - actual_paths)
    extra = sorted(actual_paths - expected_paths)
    if missing or extra:
        raise VerificationError(f"source path set mismatch: missing={missing[:8]} extra={extra[:8]}")
    base_root = base_for_inode_check
    if base_root is not None and (base_root.is_symlink() or not base_root.is_dir()):
        raise VerificationError(f"hardlink comparison root is invalid: {base_root}")
    checked = 0
    clone_inodes = set()
    for rel in sorted(expected):
        entry = expected[rel]
        kind, mode, size, digest, item_stat = found[rel]
        for field, observed in (("type", kind), ("mode", mode), ("bytes", size), ("sha256", digest)):
            target = entry[field]
            if field == "mode":
                target = oct(int(target, 8)) if isinstance(target, str) else oct(int(target))
            if observed != target:
                raise VerificationError(f"{field} mismatch for {rel}: expected {target}, got {observed}")
        if stat.S_ISREG(item_stat.st_mode):
            if item_stat.st_nlink != 1:
                raise VerificationError(f"mutable source file has multiple hard links: {rel}")
            inode = (item_stat.st_dev, item_stat.st_ino)
            if inode in clone_inodes:
                raise VerificationError(f"two source paths share one file inode: {rel}")
            clone_inodes.add(inode)
        if base_root is not None:
            base_path = base_root.joinpath(*pathlib.PurePosixPath(rel).parts)
            if not base_path.exists() and not base_path.is_symlink():
                raise VerificationError(f"base path missing during clone check: {rel}")
            base_stat = base_path.lstat()
            if (base_stat.st_dev, base_stat.st_ino) == (item_stat.st_dev, item_stat.st_ino):
                raise VerificationError(f"clone shares mutable inode with base source: {rel}")
        checked += 1
    return {"entries": checked, "parent_entries": PARENT_ENTRIES, "gitlink_entries": GITLINK_ENTRIES}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=pathlib.Path)
    parser.add_argument("--manifest", required=True, type=pathlib.Path)
    parser.add_argument("--manifest-kind", required=True, choices=("base", "final"))
    parser.add_argument("--compare-inodes-to", type=pathlib.Path)
    args = parser.parse_args()
    expected = BASE_MANIFEST_SHA if args.manifest_kind == "base" else FINAL_MANIFEST_SHA
    document = load_manifest(args.manifest, expected, args.manifest_kind)
    if sha256_file(args.source / "Cargo.lock") != LOCK_SHA:
        raise VerificationError("Cargo.lock payload SHA-256 mismatch")
    result = verify_tree(args.source, document, args.compare_inodes_to)
    print(json.dumps({"status":"verified","manifest_kind":args.manifest_kind,"manifest_sha256":expected,"commit":document["source"]["commit"],"tree":document["source"]["tree"],"cargo_lock_sha256":LOCK_SHA,**result},sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except VerificationError as exc:
        print(f"VERIFY_FAILED: {exc}", file=sys.stderr)
        raise SystemExit(2)
