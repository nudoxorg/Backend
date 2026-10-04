#!/usr/bin/env python3
"""Verify the pinned f85-to-d5a19 source packet and its materialized tree."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import sys
from collections.abc import Mapping

BASE_MANIFEST_SHA = "9d5858a15d59b1a002aae465e77c0cd40626e3920b9863049dc82a1ac7a63dd3"
FINAL_MANIFEST_SHA = "4b0a3b3adcc556fffdd0eaa899537fc515c320c63bb185603adb50d68ec96b71"
BASE_COMMIT = "f85fb905ddc499bcfe701812ab68e971f7aad097"
BASE_TREE = "23d57113d614edc95ee60de3f7a394f4baba3ad6"
FINAL_COMMIT = "d5a19a02268e45b9b0543240e50946708ba628cd"
FINAL_TREE = "e6623f06263037a5beff115746c8baa2464cee51"
GITLINK_COMMIT = "052ad81a7dbb6efed34ea482ae6c7e2e6f6682e0"
GITLINK_TREE = "8f51914241b19bb815e49fe1c4ba56fc46da6ea2"
LOCK_SHA = "5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f"
DELTA_SHA = "9124448420a3dfb36880af3001ac8c235277e78d3ce24b2f50e3af1b64e1eeab"
DELTA_BYTES = 120808
PARENT_BASE_ENTRIES = 5125
PARENT_FINAL_ENTRIES = 5126
GITLINK_ENTRIES = 4108
TOTAL_BASE_ENTRIES = PARENT_BASE_ENTRIES + GITLINK_ENTRIES
TOTAL_FINAL_ENTRIES = PARENT_FINAL_ENTRIES + GITLINK_ENTRIES
GITLINK_PATH = "server/index/turso"
NEW_PATH = "crates/library/tests/package_graph_source_witness_allocation.rs"
CHANGED_PATHS = (
    "crates/library/lib.rs",
    "crates/library/package_graph.rs",
    "crates/library/package_graph_page.rs",
    "crates/library/surface.rs",
    NEW_PATH,
    "extensions/turso/src/graph.rs",
    "extensions/turso/src/package_graph_read.rs",
    "extensions/turso/src/schema.rs",
    "extensions/turso/src/tests.rs",
)
REPLACED_PATHS = tuple(path for path in CHANGED_PATHS if path != NEW_PATH)
EXPECTED_BASE_FILES = {
    "crates/library/lib.rs": ("cbcdd75e689c34bc2f7cc9da7c22835e1e3f2242f1962f990348dfb597f62eb5", "41b8ece017b86fe305dd737f8b1acc9ee0c183e1", 16786),
    "crates/library/package_graph.rs": ("b4d8d494a6c9a9b5b1ee57c3f1d55e2dd913fba68428ce127d8e126430604913", "4eb880efb1fd5c66184c348347f0083e25dc4e86", 123078),
    "crates/library/package_graph_page.rs": ("557443f02a193a5f6f49aa3dee4fe68f6bf01b0319e17ca2b27c4661c92be5b6", "56466aaa8c61821b9cb4c22f0a6fdc21180eb868", 65867),
    "crates/library/surface.rs": ("2fe4886026cdb206ed8362b7c65d78d138dd94ff40188a2c98a405d987ad8e78", "7c42b557c97f0c00f8b605ab5c648c45f4914140", 166400),
    "extensions/turso/src/graph.rs": ("3c00316e9bceda031bf2f0652e11abdbbd91ad3c282ac7e9f69ae49ce9ddd3c6", "10d85b1da87e940065734a812db23146fc283144", 48319),
    "extensions/turso/src/package_graph_read.rs": ("1aaa0ccda878b47b63599807f624216873e4a4e768c45534c952438c7e0a1dba", "a18d364604abb10a0710d8b7926b92a5e6661c46", 58897),
    "extensions/turso/src/schema.rs": ("5e2440743ef2fda410ac00f09cda5f1c107ba71729a064851f65e4a56f9eec94", "916200b50a3e1590feb49424da38d0c835f2ab80", 5462),
    "extensions/turso/src/tests.rs": ("9374b300f3048d24a4d35999bf8b4bf728f64da48442c6aa7fdc996e07f19a72", "d54e6d925e01b7e1bb078f14da627715b272a184", 119039),
}
EXPECTED_FINAL_FILES = {
    "crates/library/lib.rs": ("b3a72b5c40848707a56d549d3e1a10268086a1e0d71318acbbff637a291e2c22", "62f3c3ebfd89b025b2cef140dd71e90830049ecd", 16883),
    "crates/library/package_graph.rs": ("26f20123dad72e26045fa2bbc5b8ee597c37b95f8ab9f06813c84251e3f45c2b", "65f37c13c9663801379bb8ef18e8f099d1319d11", 146474),
    "crates/library/package_graph_page.rs": ("68246c25e5aac8cdfc7a29ca1d5dc0a93d2a1c6ddf4039f79b68c0fa9b31819c", "deed9203b50eed4fdb13aeca033316ee8f419409", 75577),
    "crates/library/surface.rs": ("2b5bfffdb0f0cb141be40b03b23ec2399c330709e2801a6f46e4040ec59f707c", "01d69d9f1cbd568f4974ec635f2d8d68d78e3da7", 170220),
    NEW_PATH: ("b8e0925c10d2be15e329991b593fdbb143a3ba90d761ad99fb0a1c5ca6948fa7", "47e912fbf6932f8609d924589ae06c17b16d1b49", 2219),
    "extensions/turso/src/graph.rs": ("628a9411ba2ec12c7e4391edebc1ec30d070f0d6458a90db812d0a9603922846", "9cc73051c95119a0dda712a1f21d9addde4f23b9", 57236),
    "extensions/turso/src/package_graph_read.rs": ("904227a7cc885a07cc5eb9a19506c0e43c229840f4d63fd66b2666d1f125854e", "4760b5d4f9fb39b59f1d9f70bce9e0016bd831d5", 66774),
    "extensions/turso/src/schema.rs": ("919ab00251f1aafd97833b7f501b6c10b5e6ef73e26ee4270c9016cba4cfcb82", "47bf5ae9a379760f4e9ddaf51ad3501282902012", 5818),
    "extensions/turso/src/tests.rs": ("4ca3129a9890048bc101fc6ebc5cb4dac923eb8c94c745cdcde175b0224ed2a7", "d9379239e98d8ddf998235acd9c634250601827b", 142958),
}


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


def git_blob_sha1(data: bytes) -> str:
    return hashlib.sha1(b"blob " + str(len(data)).encode("ascii") + b"\0" + data).hexdigest()


def safe_relative(raw: object) -> pathlib.PurePosixPath:
    if not isinstance(raw, str) or not raw or "\x00" in raw or "\\" in raw:
        raise VerificationError(f"invalid manifest path: {raw!r}")
    rel = pathlib.PurePosixPath(raw)
    if rel.is_absolute() or rel.as_posix() != raw or any(part in ("", ".", "..") for part in rel.parts):
        raise VerificationError(f"unsafe or non-canonical manifest path: {raw!r}")
    return rel


def load_json(path: pathlib.Path, expected_sha: str) -> dict:
    if path.is_symlink() or not path.is_file():
        raise VerificationError(f"manifest is missing, non-regular, or symlinked: {path}")
    if sha256_file(path) != expected_sha:
        raise VerificationError(f"manifest SHA-256 mismatch: {path}")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise VerificationError(f"cannot decode manifest {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise VerificationError("manifest root must be an object")
    return value


def entries_by_path(document: Mapping) -> dict[str, dict]:
    source = document.get("source")
    gitlink = document.get("gitlink")
    if not isinstance(source, Mapping) or not isinstance(gitlink, Mapping):
        raise VerificationError("manifest source or gitlink section is malformed")
    source_rows = source.get("files")
    link_rows = gitlink.get("files")
    if not isinstance(source_rows, list) or not isinstance(link_rows, list):
        raise VerificationError("manifest file lists are malformed")
    expected_source_count = (
        PARENT_BASE_ENTRIES if source.get("commit") == BASE_COMMIT else PARENT_FINAL_ENTRIES
    )
    if len(source_rows) != expected_source_count or len(link_rows) != GITLINK_ENTRIES:
        raise VerificationError("manifest source/gitlink entry count mismatch")
    result: dict[str, dict] = {}
    for section_name, rows in (("source", source_rows), ("gitlink", link_rows)):
        for row in rows:
            if not isinstance(row, Mapping):
                raise VerificationError("manifest entry is not an object")
            raw_path = row.get("path")
            safe_relative(raw_path)
            if raw_path in result:
                raise VerificationError("duplicate manifest path: " + str(raw_path))
            try:
                mode = int(row["mode"], 8) if isinstance(row["mode"], str) else int(row["mode"])
                size = int(row["bytes"])
                digest = str(row["sha256"])
                blob = str(row["git_blob"]) if section_name == "source" else str(row.get("git_blob", ""))
            except (KeyError, TypeError, ValueError) as exc:
                raise VerificationError(f"malformed manifest entry: {raw_path}") from exc
            if (
                mode < 0
                or size < 0
                or len(digest) != 64
                or (section_name == "source" and len(blob) != 40)
                or (blob and len(blob) != 40)
            ):
                raise VerificationError("invalid file metadata: " + str(raw_path))
            if row.get("type") not in ("file", "symlink"):
                raise VerificationError("unsupported source entry type: " + str(raw_path))
            result[str(raw_path)] = dict(row)
    expected_total = TOTAL_BASE_ENTRIES if expected_source_count == PARENT_BASE_ENTRIES else TOTAL_FINAL_ENTRIES
    if len(result) != expected_total:
        raise VerificationError(f"expected {expected_total} total paths, got {len(result)}")
    return result


def _check_changed_row(path: str, row: Mapping, expected: tuple[str, str, int]) -> None:
    expected_sha, expected_blob, expected_size = expected
    if (
        row.get("sha256") != expected_sha
        or row.get("git_blob") != expected_blob
        or row.get("bytes") != expected_size
        or row.get("mode") != "0o644"
        or row.get("type") != "file"
    ):
        raise VerificationError("pinned changed-file metadata mismatch: " + path)


def validate_manifest_pair(base: Mapping, final: Mapping) -> None:
    base_source = base.get("source", {})
    final_source = final.get("source", {})
    if base_source.get("commit") != BASE_COMMIT or base_source.get("tree") != BASE_TREE:
        raise VerificationError("base source commit/tree mismatch")
    if final_source.get("commit") != FINAL_COMMIT or final_source.get("tree") != FINAL_TREE:
        raise VerificationError("final source commit/tree mismatch")
    if base_source.get("tracked_blob_entries") != PARENT_BASE_ENTRIES:
        raise VerificationError("base parent entry count mismatch")
    if final_source.get("tracked_blob_entries") != PARENT_FINAL_ENTRIES:
        raise VerificationError("final parent entry count mismatch")
    if base_source.get("cargo_lock_sha256") != LOCK_SHA or final_source.get("cargo_lock_sha256") != LOCK_SHA:
        raise VerificationError("Cargo.lock identity mismatch")
    for gitlink in (base.get("gitlink", {}), final.get("gitlink", {})):
        if (
            gitlink.get("path") != GITLINK_PATH
            or gitlink.get("commit") != GITLINK_COMMIT
            or gitlink.get("tree") != GITLINK_TREE
            or gitlink.get("files_count") != GITLINK_ENTRIES
            or len(gitlink.get("files", [])) != GITLINK_ENTRIES
        ):
            raise VerificationError("Turso gitlink identity/count mismatch")
    if base.get("gitlink") != final.get("gitlink"):
        raise VerificationError("Turso gitlink manifest changed")
    if set(base) - {"source", "transport"} != set(final) - {"source", "transport"}:
        raise VerificationError("unexpected top-level manifest schema change")
    for key in set(base) - {"source", "transport"}:
        if base[key] != final[key]:
            raise VerificationError("non-source manifest metadata changed: " + key)
    if set(base_source) != set(final_source):
        raise VerificationError("source manifest schema changed")
    for key in set(base_source) - {"commit", "tree", "tracked_blob_entries", "files"}:
        if base_source[key] != final_source[key]:
            raise VerificationError("source manifest metadata changed: " + key)

    before = {row["path"]: row for row in base_source["files"]}
    after = {row["path"]: row for row in final_source["files"]}
    if len(before) != PARENT_BASE_ENTRIES or len(after) != PARENT_FINAL_ENTRIES:
        raise VerificationError("source path count mismatch")
    if set(after) != set(before) | {NEW_PATH} or NEW_PATH in before:
        raise VerificationError("final source path set is not base plus the one approved new file")
    changed = [path for path in sorted(set(before) | set(after)) if before.get(path) != after.get(path)]
    if tuple(changed) != CHANGED_PATHS:
        raise VerificationError(f"source changed paths differ from the exact nine: {changed!r}")
    for path in REPLACED_PATHS:
        _check_changed_row(path, before[path], EXPECTED_BASE_FILES[path])
        _check_changed_row(path, after[path], EXPECTED_FINAL_FILES[path])
    _check_changed_row(NEW_PATH, after[NEW_PATH], EXPECTED_FINAL_FILES[NEW_PATH])
    for path in set(before) - set(CHANGED_PATHS):
        if before[path] != after[path]:
            raise VerificationError("unreviewed source file metadata changed: " + path)

    transport = final.get("transport")
    if not isinstance(transport, Mapping):
        raise VerificationError("final delta transport metadata missing")
    expected_transport = {
        "base_commit": BASE_COMMIT,
        "base_tree": BASE_TREE,
        "base_manifest_sha256": BASE_MANIFEST_SHA,
        "changed_paths": list(CHANGED_PATHS),
        "delta_archive": "source-delta.tar.gz",
        "delta_archive_sha256": DELTA_SHA,
        "delta_archive_bytes": DELTA_BYTES,
        "kind": "DELTA_OVER_VERIFIED_BASE",
        "requirements": (
            "Verify the exact f85 base source manifest, APFS-clone it privately, replace only the eight "
            "allowlisted regular files, create only the explicit new regular file inside that private clone, "
            "then verify all 9,234 source/gitlink entries, modes, hashes, Cargo.lock, gitlink, and inode isolation."
        ),
    }
    if dict(transport) != expected_transport:
        raise VerificationError("final delta transport receipt mismatch")
    entries_by_path(base)
    entries_by_path(final)


def inspect_entry(path: pathlib.Path) -> tuple[str, str, int, str, os.stat_result]:
    item_stat = path.lstat()
    if stat.S_ISREG(item_stat.st_mode):
        kind = "file"
        digest = sha256_file(path)
        size = item_stat.st_size
    elif stat.S_ISLNK(item_stat.st_mode):
        kind = "symlink"
        data = os.fsencode(os.readlink(path))
        digest = sha256_bytes(data)
        size = len(data)
    else:
        raise VerificationError("unsupported filesystem object: " + str(path))
    return kind, oct(stat.S_IMODE(item_stat.st_mode)), size, digest, item_stat


def walk_payload(root: pathlib.Path) -> dict[str, tuple[str, str, int, str, os.stat_result]]:
    if root.is_symlink() or not root.is_dir():
        raise VerificationError("source root is missing or symlinked: " + str(root))
    if (root / ".git").exists() or (root / ".git").is_symlink():
        raise VerificationError("materialized source must not contain .git metadata")
    found: dict[str, tuple[str, str, int, str, os.stat_result]] = {}
    stack = [(root, pathlib.PurePosixPath())]
    while stack:
        directory, prefix = stack.pop()
        with os.scandir(directory) as scan:
            items = sorted(scan, key=lambda item: item.name)
        for item in items:
            child = pathlib.Path(item.path)
            relative = (prefix / item.name).as_posix()
            safe_relative(relative)
            info = item.stat(follow_symlinks=False)
            if stat.S_ISDIR(info.st_mode):
                stack.append((child, pathlib.PurePosixPath(relative)))
            else:
                if relative in found:
                    raise VerificationError("duplicate filesystem path: " + relative)
                found[relative] = inspect_entry(child)
    return found


def verify_tree(
    root: pathlib.Path,
    document: Mapping,
    base_for_inode_check: pathlib.Path | None = None,
) -> dict:
    expected = entries_by_path(document)
    found = walk_payload(root)
    missing = sorted(set(expected) - set(found))
    extra = sorted(set(found) - set(expected))
    if missing or extra:
        raise VerificationError(f"source path set mismatch: missing={missing[:8]} extra={extra[:8]}")
    if base_for_inode_check is not None and (
        base_for_inode_check.is_symlink() or not base_for_inode_check.is_dir()
    ):
        raise VerificationError("inode comparison root is invalid")
    unique_inodes: set[tuple[int, int]] = set()
    for relative in sorted(expected):
        entry = expected[relative]
        kind, mode, size, digest, item_stat = found[relative]
        for key, actual in (("type", kind), ("mode", mode), ("bytes", size), ("sha256", digest)):
            raw_expected = entry[key]
            if key == "mode":
                raw_expected = oct(int(raw_expected, 8)) if isinstance(raw_expected, str) else oct(int(raw_expected))
            if actual != raw_expected:
                raise VerificationError(f"{key} mismatch for {relative}: expected {raw_expected}, got {actual}")
        if stat.S_ISREG(item_stat.st_mode):
            if item_stat.st_nlink != 1:
                raise VerificationError("source file has multiple hard links: " + relative)
            inode = (item_stat.st_dev, item_stat.st_ino)
            if inode in unique_inodes:
                raise VerificationError("two source files share one inode: " + relative)
            unique_inodes.add(inode)
        if base_for_inode_check is not None:
            base_path = base_for_inode_check.joinpath(*pathlib.PurePosixPath(relative).parts)
            try:
                base_stat = base_path.lstat()
            except FileNotFoundError:
                if relative != NEW_PATH:
                    raise VerificationError("base path missing during clone check: " + relative)
            else:
                if stat.S_ISREG(item_stat.st_mode) and (
                    base_stat.st_dev,
                    base_stat.st_ino,
                ) == (item_stat.st_dev, item_stat.st_ino):
                    raise VerificationError("clone shares an inode with base source: " + relative)
    return {
        "entries": len(expected),
        "parent_entries": PARENT_FINAL_ENTRIES if len(expected) == TOTAL_FINAL_ENTRIES else PARENT_BASE_ENTRIES,
        "gitlink_entries": GITLINK_ENTRIES,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=pathlib.Path)
    parser.add_argument("--manifest", required=True, type=pathlib.Path)
    parser.add_argument("--manifest-kind", required=True, choices=("base", "final"))
    parser.add_argument("--base-manifest", required=True, type=pathlib.Path)
    parser.add_argument("--compare-inodes-to", type=pathlib.Path)
    args = parser.parse_args()
    base = load_json(args.base_manifest, BASE_MANIFEST_SHA)
    if args.manifest_kind == "base":
        document = base
        expected_commit, expected_tree, expected_sha = BASE_COMMIT, BASE_TREE, BASE_MANIFEST_SHA
    else:
        final = load_json(args.manifest, FINAL_MANIFEST_SHA)
        validate_manifest_pair(base, final)
        document = final
        expected_commit, expected_tree, expected_sha = FINAL_COMMIT, FINAL_TREE, FINAL_MANIFEST_SHA
    if document["source"]["commit"] != expected_commit or document["source"]["tree"] != expected_tree:
        raise VerificationError("manifest identity does not match requested kind")
    lock_path = args.source / "Cargo.lock"
    if lock_path.is_symlink() or not lock_path.is_file() or sha256_file(lock_path) != LOCK_SHA:
        raise VerificationError("Cargo.lock payload hash mismatch")
    result = verify_tree(args.source, document, args.compare_inodes_to)
    print(json.dumps({
        "status": "verified",
        "manifest_kind": args.manifest_kind,
        "manifest_sha256": expected_sha,
        "commit": expected_commit,
        "tree": expected_tree,
        "cargo_lock_sha256": LOCK_SHA,
        **result,
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except VerificationError as exc:
        print("VERIFY_FAILED: " + str(exc), file=sys.stderr)
        raise SystemExit(2)
