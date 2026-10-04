#!/usr/bin/env python3
"""Build the local-only f85-to-graph-java source delta from pinned Git objects."""
from __future__ import annotations

import copy
import gzip
import hashlib
import io
import json
import os
import pathlib
import subprocess
import tarfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
PACKET = ROOT / "packet"
REPOSITORY = pathlib.Path("/private/tmp/nudox-canonical-integration-20261004")
BASE_COMMIT = "f85fb905ddc499bcfe701812ab68e971f7aad097"
BASE_TREE = "23d57113d614edc95ee60de3f7a394f4baba3ad6"
BASE_MANIFEST = pathlib.Path(
    "/private/tmp/nudox-semantic-fixture-repair-20261004T141414Z/"
    "packet-preparation-v2/packet/source-verification.json"
)
BASE_MANIFEST_SHA = "9d5858a15d59b1a002aae465e77c0cd40626e3920b9863049dc82a1ac7a63dd3"
SOURCE_RECEIPT = pathlib.Path("/private/tmp/nudox-latest-gui-index-candidate.json")
SOURCE_RECEIPT_SHA = "63842e36d61e39e386e839a3252b8e647d5ed92541e9942457952de60faa30a7"
VERIFIER = ROOT / "helpers" / "verify_source_manifest.py"
BASE_CANDIDATE = "d5a19a02268e45b9b0543240e50946708ba628cd"
BASE_CANDIDATE_TREE = "e6623f06263037a5beff115746c8baa2464cee51"
JAVA_FIXTURE = "0e18c888f70de46c410ddb7333391d661a94d650"
JAVA_FIXTURE_TREE = "bd5a9802d89661d1fa43a10fa3336d3a7ab33146"
COMMIT = "40abfeac823123ae752261e3eccffbaed978c994"
TREE = "cab3284ce01237a15268c90936f01930ed6d4c35"
SOURCE_PATCH_SHA = "89ca1b4787675084b42f37ca73af69a90d092facf3343c959b8f0e93157fbb38"
LOCK_SHA = "5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f"
GITLINK_PATH = "server/index/turso"
GITLINK_COMMIT = "052ad81a7dbb6efed34ea482ae6c7e2e6f6682e0"
GITLINK_TREE = "8f51914241b19bb815e49fe1c4ba56fc46da6ea2"
PARENT_BASE_ENTRIES = 5125
PARENT_FINAL_ENTRIES = 5126
GITLINK_ENTRIES = 4108
TOTAL_BASE_ENTRIES = PARENT_BASE_ENTRIES + GITLINK_ENTRIES
TOTAL_FINAL_ENTRIES = PARENT_FINAL_ENTRIES + GITLINK_ENTRIES
NEW_PATHS = ("crates/library/tests/package_graph_source_witness_allocation.rs",)
CHANGED_PATHS = tuple(sorted((
    "crates/library/lib.rs",
    "crates/library/package_graph.rs",
    "crates/library/package_graph_page.rs",
    "crates/library/semantic_shape.rs",
    "crates/library/surface.rs",
    *NEW_PATHS,
    "extensions/turso/src/graph.rs",
    "extensions/turso/src/package_graph_read.rs",
    "extensions/turso/src/schema.rs",
    "extensions/turso/src/tests.rs",
)))
REPLACED_PATHS = tuple(path for path in CHANGED_PATHS if path not in NEW_PATHS)


class PacketError(RuntimeError):
    pass


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def git_blob(data: bytes) -> str:
    header = b"blob " + str(len(data)).encode("ascii") + b"\0"
    return hashlib.sha1(header + data).hexdigest()


def git(*args: str) -> bytes:
    completed = subprocess.run(
        ["git", "-C", str(REPOSITORY), *args],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.returncode != 0:
        raise PacketError("Git object query failed: " + completed.stderr.decode("utf-8", "replace"))
    return completed.stdout


def git_tree(commit: str) -> dict[str, tuple[str, str, str]]:
    raw = git("ls-tree", "-r", "-z", "--full-tree", commit)
    result: dict[str, tuple[str, str, str]] = {}
    for record in raw.split(b"\0"):
        if not record:
            continue
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, oid = metadata.decode("ascii").split(" ")
        path = raw_path.decode("utf-8")
        if path in result:
            raise PacketError("duplicate Git tree path: " + path)
        result[path] = (mode, kind, oid)
    return result


def blob(commit: str, path: str) -> bytes:
    return git("cat-file", "blob", f"{commit}:{path}")


def file_row(path: str, mode: str, kind: str, oid: str, data: bytes) -> dict:
    if kind != "blob" or mode not in ("100644", "100755", "120000"):
        raise PacketError("source entry is not a supported Git blob: " + path)
    if git_blob(data) != oid:
        raise PacketError("Git blob bytes differ from the pinned tree object: " + path)
    if mode == "120000":
        entry_type, file_mode = "symlink", "0o755"
    else:
        entry_type = "file"
        file_mode = "0o755" if mode == "100755" else "0o644"
    return {
        "bytes": len(data),
        "git_blob": oid,
        "mode": file_mode,
        "path": path,
        "sha256": sha256(data),
        "type": entry_type,
    }


def tree_statuses() -> dict[str, str]:
    raw = git("diff", "--name-status", "--no-renames", BASE_COMMIT, COMMIT)
    statuses: dict[str, str] = {}
    for line in raw.decode("utf-8").splitlines():
        status, path = line.split("\t", 1)
        if path in statuses:
            raise PacketError("duplicate candidate diff path: " + path)
        statuses[path] = status
    return statuses


def check_base_tree(base_manifest: dict, base_tree: dict[str, tuple[str, str, str]]) -> None:
    source = base_manifest["source"]
    rows = {row["path"]: row for row in source["files"]}
    tree_rows = {path: row for path, row in base_tree.items() if path != GITLINK_PATH}
    if len(rows) != PARENT_BASE_ENTRIES or set(rows) != set(tree_rows):
        raise PacketError("pinned f85 manifest differs from its Git parent tree")
    for path, row in rows.items():
        mode, kind, oid = tree_rows[path]
        if row.get("git_blob") != oid or kind != "blob":
            raise PacketError("f85 manifest blob mismatch: " + path)
        if mode == "120000":
            expected_type, expected_mode = "symlink", "0o755"
        else:
            expected_type = "file"
            expected_mode = "0o755" if mode == "100755" else "0o644"
        if row.get("type") != expected_type or row.get("mode") != expected_mode:
            raise PacketError("f85 manifest mode/type mismatch: " + path)
    link = base_tree.get(GITLINK_PATH)
    if link != ("160000", "commit", GITLINK_COMMIT):
        raise PacketError("f85 superproject tree has a different Turso gitlink")


def write_exclusive(path: pathlib.Path, data: bytes, mode: int = 0o600) -> None:
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(path, mode)


def deterministic_archive(contents: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in CHANGED_PATHS:
            data = contents[path]
            info = tarfile.TarInfo("source/" + path)
            info.type = tarfile.REGTYPE
            info.mode = 0o644
            info.size = len(data)
            info.mtime = 0
            info.uid = 0
            info.gid = 0
            info.uname = ""
            info.gname = ""
            archive.addfile(info, io.BytesIO(data))
    output = io.BytesIO()
    with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=0, compresslevel=9) as zipped:
        zipped.write(buffer.getvalue())
    return output.getvalue()


def main() -> None:
    if PACKET.is_symlink() or not PACKET.is_dir() or any(PACKET.iterdir()):
        raise PacketError("refusing a missing, symlinked, or nonempty packet directory")
    source_receipt_bytes = SOURCE_RECEIPT.read_bytes()
    source_receipt = json.loads(source_receipt_bytes)
    if sha256(source_receipt_bytes) != SOURCE_RECEIPT_SHA:
        raise PacketError("source candidate receipt hash mismatch")
    graph_java = source_receipt.get("graph_java", {})
    java_fixture = source_receipt.get("java", {})
    combined_hashes = source_receipt.get("combined", {}).get("source_sha256", {})
    base_bytes = BASE_MANIFEST.read_bytes()
    base_sha = sha256(base_bytes)
    if base_sha != BASE_MANIFEST_SHA:
        raise PacketError("pinned base manifest hash mismatch")
    if (
        source_receipt.get("base") != BASE_COMMIT
        or graph_java != {"commit": COMMIT, "tree": TREE, "parent": BASE_CANDIDATE}
        or java_fixture.get("commit") != JAVA_FIXTURE
        or java_fixture.get("tree") != JAVA_FIXTURE_TREE
        or source_receipt.get("main_merge_index_preserved") is not True
        or set(combined_hashes) != set(source_receipt.get("combined", {}).get("changed_paths", []))
    ):
        raise PacketError("source receipt graph/java identity or provenance mismatch")
    if git("rev-parse", f"{BASE_COMMIT}^{{tree}}").decode().strip() != BASE_TREE:
        raise PacketError("base commit/tree mismatch")
    if git("rev-parse", f"{COMMIT}^{{tree}}").decode().strip() != TREE:
        raise PacketError("candidate commit/tree mismatch")
    if git("rev-parse", f"{BASE_CANDIDATE}^{{tree}}").decode().strip() != BASE_CANDIDATE_TREE:
        raise PacketError("d5a19 graph candidate tree mismatch")
    if git("rev-parse", f"{JAVA_FIXTURE}^{{tree}}").decode().strip() != JAVA_FIXTURE_TREE:
        raise PacketError("reviewed Java fixture tree mismatch")
    if git("rev-parse", f"{COMMIT}^").decode().strip() != BASE_CANDIDATE:
        raise PacketError("graph/java candidate parent is not d5a19")
    java_blob = git("rev-parse", f"{JAVA_FIXTURE}:crates/library/semantic_shape.rs").decode().strip()
    final_semantic_blob = git("rev-parse", f"{COMMIT}:crates/library/semantic_shape.rs").decode().strip()
    if java_blob != final_semantic_blob:
        raise PacketError("candidate semantic_shape.rs is not the exact reviewed Java fixture blob")
    java_overlay = git("diff", "--name-status", "--no-renames", BASE_CANDIDATE, COMMIT)
    if java_overlay.decode().splitlines() != ["M\tcrates/library/semantic_shape.rs"]:
        raise PacketError("graph/java overlay is not exactly the semantic_shape.rs replacement")

    base_manifest = json.loads(base_bytes)
    if (
        base_manifest.get("source", {}).get("commit") != BASE_COMMIT
        or base_manifest.get("source", {}).get("tree") != BASE_TREE
        or base_manifest.get("source", {}).get("cargo_lock_sha256") != LOCK_SHA
        or len(base_manifest.get("source", {}).get("files", [])) != PARENT_BASE_ENTRIES
        or len(base_manifest.get("gitlink", {}).get("files", [])) != GITLINK_ENTRIES
        or base_manifest.get("gitlink", {}).get("commit") != GITLINK_COMMIT
        or base_manifest.get("gitlink", {}).get("tree") != GITLINK_TREE
    ):
        raise PacketError("f85 base manifest identity/count/lock/gitlink mismatch")
    base_tree = git_tree(BASE_COMMIT)
    candidate_tree = git_tree(COMMIT)
    check_base_tree(base_manifest, base_tree)
    base_source_paths = set(base_manifest["source"]["files"][i]["path"] for i in range(PARENT_BASE_ENTRIES))
    source_tree = {path: row for path, row in candidate_tree.items() if path != GITLINK_PATH}
    if set(source_tree) != base_source_paths | set(NEW_PATHS):
        raise PacketError("candidate Git tree has unreviewed source additions or removals")
    statuses = tree_statuses()
    expected_statuses = {path: "M" for path in REPLACED_PATHS} | {NEW_PATHS[0]: "A"}
    if statuses != expected_statuses:
        raise PacketError("candidate Git diff path/status set differs from the exact reviewed ten")
    candidate_link = candidate_tree.get(GITLINK_PATH)
    if candidate_link != base_tree.get(GITLINK_PATH) or candidate_link != (
        "160000", "commit", GITLINK_COMMIT
    ):
        raise PacketError("candidate changed the Turso gitlink")

    base_lock = blob(BASE_COMMIT, "Cargo.lock")
    candidate_lock = blob(COMMIT, "Cargo.lock")
    if sha256(candidate_lock) != LOCK_SHA or git_blob(base_lock) != git_blob(candidate_lock):
        raise PacketError("Cargo.lock differs from the pinned base")

    selected_source_hashes = {path: combined_hashes.get(path) for path in CHANGED_PATHS}
    if any(not isinstance(value, str) or len(value) != 64 for value in selected_source_hashes.values()):
        raise PacketError("source receipt does not provide all ten selected source hashes")
    selected_git_blobs = {path: candidate_tree[path][2] for path in CHANGED_PATHS}
    selected_receipt = {
        "format": "selected-source-candidate-v1",
        "source_receipt_sha256": SOURCE_RECEIPT_SHA,
        "selected_section": "graph_java",
        "base": BASE_COMMIT,
        "base_tree": BASE_TREE,
        "graph_java_commit": COMMIT,
        "graph_java_tree": TREE,
        "graph_java_parent": BASE_CANDIDATE,
        "graph_candidate_commit": BASE_CANDIDATE,
        "graph_candidate_tree": BASE_CANDIDATE_TREE,
        "java_fixture_commit": JAVA_FIXTURE,
        "java_fixture_tree": JAVA_FIXTURE_TREE,
        "java_fixture_semantic_shape_blob": java_blob,
        "source_delta_patch_sha256": SOURCE_PATCH_SHA,
        "paths": list(CHANGED_PATHS),
        "source_sha256": selected_source_hashes,
        "source_git_blob": selected_git_blobs,
        "main_merge_index_preserved": True,
    }
    selected_receipt_bytes = (json.dumps(selected_receipt, sort_keys=True, indent=2) + "\n").encode()
    selected_receipt_sha = sha256(selected_receipt_bytes)

    base_rows = {row["path"]: row for row in base_manifest["source"]["files"]}
    final_rows = copy.deepcopy(base_rows)
    contents: dict[str, bytes] = {}
    for path in CHANGED_PATHS:
        mode, kind, oid = candidate_tree[path]
        if mode != "100644" or kind != "blob":
            raise PacketError("reviewed candidate path is not a 0644 regular Git blob: " + path)
        data = blob(COMMIT, path)
        row = file_row(path, mode, kind, oid, data)
        if row["sha256"] != selected_source_hashes.get(path):
            raise PacketError("candidate blob SHA-256 differs from selected source receipt: " + path)
        contents[path] = data
        final_rows[path] = row
    if set(final_rows) != base_source_paths | set(NEW_PATHS):
        raise PacketError("final source path set is not base plus the one explicit addition")

    patch_bytes = git("diff", "--binary", BASE_COMMIT, COMMIT, "--", *CHANGED_PATHS)
    if sha256(patch_bytes) != SOURCE_PATCH_SHA:
        raise PacketError("pinned ten-path source diff hash mismatch")
    archive_bytes = deterministic_archive(contents)
    archive_sha = sha256(archive_bytes)
    final_manifest = copy.deepcopy(base_manifest)
    final_manifest["source"]["commit"] = COMMIT
    final_manifest["source"]["tree"] = TREE
    final_manifest["source"]["tracked_blob_entries"] = PARENT_FINAL_ENTRIES
    final_manifest["source"]["files"] = [final_rows[path] for path in sorted(final_rows)]
    final_manifest["transport"] = {
        "base_commit": BASE_COMMIT,
        "base_tree": BASE_TREE,
        "base_manifest_sha256": BASE_MANIFEST_SHA,
        "changed_paths": list(CHANGED_PATHS),
        "delta_archive": "source-delta.tar.gz",
        "delta_archive_sha256": archive_sha,
        "delta_archive_bytes": len(archive_bytes),
        "kind": "DELTA_OVER_VERIFIED_BASE",
        "requirements": (
            "Verify the exact f85 base source manifest, APFS-clone it privately, replace only the nine "
            "allowlisted regular files, create only the explicit new regular file inside that private clone, "
            "then verify all 9,234 source/gitlink entries, modes, hashes, Cargo.lock, gitlink, and inode isolation."
        ),
    }
    final_manifest_bytes = (json.dumps(final_manifest, sort_keys=True, indent=2) + "\n").encode()
    final_manifest_sha = sha256(final_manifest_bytes)
    if VERIFIER.is_symlink() or not VERIFIER.is_file():
        raise PacketError("source manifest verifier is missing or not a regular file")
    verifier_sha = sha256(VERIFIER.read_bytes())
    changed_files = []
    for path in CHANGED_PATHS:
        before = base_rows.get(path)
        after = final_rows[path]
        changed_files.append({
            "path": path,
            "status": "added" if before is None else "replaced",
            "base_sha256": None if before is None else before["sha256"],
            "base_git_blob": None if before is None else before["git_blob"],
            "source_sha256": after["sha256"],
            "git_blob": after["git_blob"],
            "bytes": after["bytes"],
            "mode": after["mode"],
        })
    package = {
        "format": "source-delta-packet-v1",
        "base": BASE_COMMIT,
        "base_tree": BASE_TREE,
        "base_manifest_sha256": BASE_MANIFEST_SHA,
        "candidate": COMMIT,
        "tree": TREE,
        "candidate_receipt_sha256": selected_receipt_sha,
        "source_receipt_sha256": SOURCE_RECEIPT_SHA,
        "source_delta_patch_sha256": SOURCE_PATCH_SHA,
        "verifier_sha256": verifier_sha,
        "changed_paths": list(CHANGED_PATHS),
        "new_paths": list(NEW_PATHS),
        "changed_files": changed_files,
        "parent_blob_entries": PARENT_FINAL_ENTRIES,
        "gitlink_blob_entries": GITLINK_ENTRIES,
        "total_entries": TOTAL_FINAL_ENTRIES,
        "cargo_lock_sha256": LOCK_SHA,
        "gitlink_commit": GITLINK_COMMIT,
        "gitlink_tree": GITLINK_TREE,
        "manifest_sha256": final_manifest_sha,
        "delta_sha256": archive_sha,
        "delta_bytes": len(archive_bytes),
        "no_transfer_or_remote_mutation": True,
        "no_cargo_or_build_or_layout": True,
    }
    outputs = {
        "base-source-verification.json": base_bytes,
        "candidate-receipt.json": selected_receipt_bytes,
        "source-delta.patch": patch_bytes,
        "source-delta.tar.gz": archive_bytes,
        "source-verification.json": final_manifest_bytes,
        "package-receipt.json": (json.dumps(package, sort_keys=True, indent=2) + "\n").encode(),
    }
    for name, data in outputs.items():
        write_exclusive(PACKET / name, data)
    print(json.dumps({
        "status": "local_packet_prepared",
        "base": BASE_COMMIT,
        "candidate": COMMIT,
        "tree": TREE,
        "changed_paths": list(CHANGED_PATHS),
        "base_entries": TOTAL_BASE_ENTRIES,
        "final_entries": TOTAL_FINAL_ENTRIES,
        "delta_sha256": archive_sha,
        "delta_bytes": len(archive_bytes),
        "base_manifest_sha256": BASE_MANIFEST_SHA,
        "final_manifest_sha256": final_manifest_sha,
        "candidate_receipt_sha256": selected_receipt_sha,
        "source_receipt_sha256": SOURCE_RECEIPT_SHA,
    }, sort_keys=True))


if __name__ == "__main__":
    try:
        main()
    except (PacketError, OSError, KeyError, ValueError, json.JSONDecodeError, tarfile.TarError) as exc:
        raise SystemExit("PACKET_BUILD_FAILED: " + str(exc))
