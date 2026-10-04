#!/usr/bin/env python3
"""Materialize the reviewed graph/java source packet over a verified f85 APFS clone."""
from __future__ import annotations

import argparse
import ctypes
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tarfile
import time
from collections.abc import Mapping

HERE = pathlib.Path(__file__).resolve().parent
PACKET_SHA = "93785f37cad9ae0e11dd1e537e34714d50ac2dba4a158354f5f7083b79f0f2ac"
CANDIDATE_SHA = "4b5b64d90eda9a90c8813433e96523f2e097657ea3e7f7dca1bc6af512b826bb"
SOURCE_RECEIPT_SHA = "63842e36d61e39e386e839a3252b8e647d5ed92541e9942457952de60faa30a7"
DELTA_SHA = "453dd60d7f8543332855cd3259adf8a1d8b0b73b88bca6eb11feb2ae8c420966"
DELTA_BYTES = 135303
BASE_MANIFEST_SHA = "9d5858a15d59b1a002aae465e77c0cd40626e3920b9863049dc82a1ac7a63dd3"
FINAL_MANIFEST_SHA = "29a94e89831913ac4b7a0a9b0723324a5e9f675e390be077f2a796409e63a8f3"
COMMIT = "40abfeac823123ae752261e3eccffbaed978c994"
TREE = "cab3284ce01237a15268c90936f01930ed6d4c35"
BASE_COMMIT = "f85fb905ddc499bcfe701812ab68e971f7aad097"
BASE_TREE = "23d57113d614edc95ee60de3f7a394f4baba3ad6"
BASE_CANDIDATE = "d5a19a02268e45b9b0543240e50946708ba628cd"
BASE_CANDIDATE_TREE = "e6623f06263037a5beff115746c8baa2464cee51"
JAVA_FIXTURE = "0e18c888f70de46c410ddb7333391d661a94d650"
JAVA_FIXTURE_TREE = "bd5a9802d89661d1fa43a10fa3336d3a7ab33146"
LOCK_SHA = "5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f"
GITLINK_COMMIT = "052ad81a7dbb6efed34ea482ae6c7e2e6f6682e0"
GITLINK_TREE = "8f51914241b19bb815e49fe1c4ba56fc46da6ea2"
SOURCE_PATCH_SHA = "89ca1b4787675084b42f37ca73af69a90d092facf3343c959b8f0e93157fbb38"
VERIFIER_SHA = "fc13263fddad797774d832e4bdd3e6e029f788e1b588769c3427459fa811b96e"
PARENT_BASE_ENTRIES = 5125
PARENT_FINAL_ENTRIES = 5126
GITLINK_ENTRIES = 4108
BASE_ENTRIES = PARENT_BASE_ENTRIES + GITLINK_ENTRIES
FINAL_ENTRIES = PARENT_FINAL_ENTRIES + GITLINK_ENTRIES
NEW_PATH = "crates/library/tests/package_graph_source_witness_allocation.rs"
CHANGED_PATHS = (
    "crates/library/lib.rs",
    "crates/library/package_graph.rs",
    "crates/library/package_graph_page.rs",
    "crates/library/semantic_shape.rs",
    "crates/library/surface.rs",
    NEW_PATH,
    "extensions/turso/src/graph.rs",
    "extensions/turso/src/package_graph_read.rs",
    "extensions/turso/src/schema.rs",
    "extensions/turso/src/tests.rs",
)
REPLACED_PATHS = tuple(path for path in CHANGED_PATHS if path != NEW_PATH)


class StageError(RuntimeError):
    pass


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_json(path: pathlib.Path) -> dict:
    if path.is_symlink() or not path.is_file():
        raise StageError("packet member missing or not regular: " + str(path))
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise StageError("cannot decode packet JSON: " + str(exc)) from exc
    if not isinstance(value, dict):
        raise StageError("packet JSON root must be an object")
    return value


def check_owned_directory(path: pathlib.Path, require_private: bool) -> None:
    if path.is_symlink() or not path.is_dir():
        raise StageError("directory missing or symlinked: " + str(path))
    info = path.lstat()
    if info.st_uid != os.getuid():
        raise StageError("directory is not owned by the current user: " + str(path))
    mode = stat.S_IMODE(info.st_mode)
    if require_private and mode != 0o700:
        raise StageError("private directory must have mode 0700: " + str(path))
    if not require_private and mode & 0o022:
        raise StageError("directory is group/world writable: " + str(path))


def check_home_descendants(path: pathlib.Path) -> None:
    home = pathlib.Path.home()
    try:
        relative = path.relative_to(home)
    except ValueError as exc:
        raise StageError("source release paths must be inside the current user's home") from exc
    check_owned_directory(home, require_private=False)
    current = home
    for part in relative.parts:
        current = current / part
        check_owned_directory(current, require_private=False)


def rename_exclusive(source: pathlib.Path, target: pathlib.Path) -> None:
    if sys.platform != "darwin":
        raise StageError("exclusive atomic publish is available only on the reviewed macOS host")
    libc = ctypes.CDLL(None, use_errno=True)
    operation = getattr(libc, "renamex_np", None)
    if operation is None:
        raise StageError("Darwin renamex_np unavailable; refusing non-exclusive publish")
    operation.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    operation.restype = ctypes.c_int
    if operation(os.fsencode(str(source)), os.fsencode(str(target)), 0x00000004) != 0:
        code = ctypes.get_errno()
        raise OSError(code, os.strerror(code), str(target))


def safe_rel(raw: object) -> pathlib.PurePosixPath:
    if not isinstance(raw, str) or not raw or "\x00" in raw or "\\" in raw:
        raise StageError("invalid archive or manifest path")
    result = pathlib.PurePosixPath(raw)
    if result.is_absolute() or result.as_posix() != raw or any(part in ("", ".", "..") for part in result.parts):
        raise StageError("unsafe or non-canonical path: " + repr(raw))
    return result


def manifest_entries(document: Mapping) -> dict[str, dict]:
    result: dict[str, dict] = {}
    for section in ("source", "gitlink"):
        for entry in document[section]["files"]:
            path = entry.get("path")
            safe_rel(path)
            if path in result:
                raise StageError("duplicate manifest path: " + str(path))
            result[str(path)] = dict(entry)
    expected = BASE_ENTRIES if document["source"]["commit"] == BASE_COMMIT else FINAL_ENTRIES
    if len(result) != expected:
        raise StageError("manifest total entry count mismatch")
    return result


def validate_manifest_delta(base: Mapping, final: Mapping, candidate: Mapping, package: Mapping) -> None:
    try:
        from verify_source_manifest import (
            CHANGED_PATHS as VERIFIED_PATHS,
            NEW_PATH as VERIFIED_NEW_PATH,
            VerificationError,
            validate_manifest_pair,
        )
        validate_manifest_pair(base, final)
    except (ImportError, VerificationError) as exc:
        raise StageError("source manifest pair rejected: " + str(exc)) from exc
    if VERIFIED_PATHS != CHANGED_PATHS or VERIFIED_NEW_PATH != NEW_PATH:
        raise StageError("stager and verifier allowlists differ")
    if (
        candidate.get("format") != "selected-source-candidate-v1"
        or candidate.get("graph_java_commit") != COMMIT
        or candidate.get("graph_java_tree") != TREE
        or candidate.get("graph_java_parent") != BASE_CANDIDATE
        or candidate.get("graph_candidate_commit") != BASE_CANDIDATE
        or candidate.get("graph_candidate_tree") != BASE_CANDIDATE_TREE
        or candidate.get("java_fixture_commit") != JAVA_FIXTURE
        or candidate.get("java_fixture_tree") != JAVA_FIXTURE_TREE
        or candidate.get("java_fixture_semantic_shape_blob") != "37cf8e39b27d7923eea67a5f9158cc96e659db7e"
        or candidate.get("base") != BASE_COMMIT
        or candidate.get("base_tree") != BASE_TREE
        or candidate.get("source_receipt_sha256") != SOURCE_RECEIPT_SHA
        or candidate.get("main_merge_index_preserved") is not True
    ):
        raise StageError("candidate receipt identity mismatch")
    if candidate.get("paths") != list(CHANGED_PATHS) or candidate.get("source_delta_patch_sha256") != SOURCE_PATCH_SHA:
        raise StageError("selected receipt path/patch identity mismatch")
    final_rows = {row["path"]: row for row in final["source"]["files"]}
    if candidate.get("source_sha256") != {
        path: final_rows[path]["sha256"] for path in CHANGED_PATHS if path in final_rows
    }:
        raise StageError("candidate source hash map differs from final manifest")
    if candidate.get("source_git_blob") != {
        path: final_rows[path]["git_blob"] for path in CHANGED_PATHS if path in final_rows
    }:
        raise StageError("candidate Git blob map differs from final manifest")
    if package.get("format") != "source-delta-packet-v1":
        raise StageError("unsupported package receipt format")
    if (
        package.get("base") != BASE_COMMIT
        or package.get("base_tree") != BASE_TREE
        or package.get("candidate") != COMMIT
        or package.get("tree") != TREE
        or package.get("changed_paths") != list(CHANGED_PATHS)
        or package.get("new_paths") != [NEW_PATH]
        or package.get("parent_blob_entries") != PARENT_FINAL_ENTRIES
        or package.get("gitlink_blob_entries") != GITLINK_ENTRIES
        or package.get("total_entries") != FINAL_ENTRIES
        or package.get("cargo_lock_sha256") != LOCK_SHA
        or package.get("gitlink_commit") != GITLINK_COMMIT
        or package.get("gitlink_tree") != GITLINK_TREE
        or package.get("manifest_sha256") != FINAL_MANIFEST_SHA
        or package.get("delta_sha256") != DELTA_SHA
        or package.get("delta_bytes") != DELTA_BYTES
        or package.get("candidate_receipt_sha256") != CANDIDATE_SHA
        or package.get("source_receipt_sha256") != SOURCE_RECEIPT_SHA
        or package.get("source_delta_patch_sha256") != SOURCE_PATCH_SHA
        or package.get("verifier_sha256") != VERIFIER_SHA
        or package.get("no_transfer_or_remote_mutation") is not True
        or package.get("no_cargo_or_build_or_layout") is not True
    ):
        raise StageError("package receipt identity, scope, or pin mismatch")
    base_rows = {row["path"]: row for row in base["source"]["files"]}
    final_rows = {row["path"]: row for row in final["source"]["files"]}
    rows = package.get("changed_files")
    if not isinstance(rows, list) or [row.get("path") for row in rows] != list(CHANGED_PATHS):
        raise StageError("package changed-file row paths mismatch")
    for row in rows:
        path = row["path"]
        before = base_rows.get(path)
        after = final_rows.get(path)
        if after is None:
            raise StageError("package candidate path missing from final manifest: " + path)
        if before is None:
            if path != NEW_PATH or row.get("status") != "added":
                raise StageError("only the explicit new regular path may be added")
            if row.get("base_sha256") is not None or row.get("base_git_blob") is not None:
                raise StageError("new path unexpectedly claims a base file")
        else:
            if row.get("status") != "replaced":
                raise StageError("replacement status mismatch: " + path)
            if row.get("base_sha256") != before.get("sha256") or row.get("base_git_blob") != before.get("git_blob"):
                raise StageError("base file receipt mismatch: " + path)
        if (
            row.get("source_sha256") != after.get("sha256")
            or row.get("git_blob") != after.get("git_blob")
            or row.get("bytes") != after.get("bytes")
            or row.get("mode") != "0o644"
            or after.get("mode") != "0o644"
            or after.get("type") != "file"
        ):
            raise StageError("candidate file receipt mismatch: " + path)


def _ensure_real_parents(root: pathlib.Path, relative: pathlib.PurePosixPath) -> pathlib.Path:
    cursor = root
    for part in relative.parts[:-1]:
        cursor = cursor / part
        try:
            info = cursor.lstat()
        except FileNotFoundError as exc:
            raise StageError("overlay parent directory is missing: " + str(cursor)) from exc
        if not stat.S_ISDIR(info.st_mode):
            raise StageError("overlay parent is not a real directory: " + str(cursor))
    return root.joinpath(*relative.parts)


def overlay_files(
    archive: pathlib.Path,
    source_root: pathlib.Path,
    changed_paths: list[str],
    entries: Mapping[str, dict],
    base_entries: Mapping[str, dict],
) -> None:
    if tuple(changed_paths) != CHANGED_PATHS:
        raise StageError("candidate must name exactly the ten reviewed source paths")
    expected = set(CHANGED_PATHS)
    if archive.is_symlink() or not archive.is_file() or sha256_file(archive) != DELTA_SHA or archive.stat().st_size != DELTA_BYTES:
        raise StageError("source delta archive hash/size mismatch")
    with tarfile.open(str(archive), mode="r:gz") as tar:
        members = tar.getmembers()
        by_path: dict[str, tarfile.TarInfo] = {}
        for member in members:
            prefix = "source/"
            if not member.name.startswith(prefix):
                raise StageError("unexpected archive member prefix: " + repr(member.name))
            rel = member.name[len(prefix):]
            safe_rel(rel)
            if rel in by_path:
                raise StageError("duplicate archive member: " + rel)
            if not member.isfile() or member.issym() or member.islnk() or member.mode != 0o644:
                raise StageError("archive member is not a 0644 regular file: " + rel)
            if rel not in expected:
                raise StageError("unknown archive path: " + rel)
            by_path[rel] = member
        if set(by_path) != expected or len(members) != len(expected):
            raise StageError("archive path set differs from the exact ten-file delta")

        targets: dict[str, tuple[pathlib.Path, tarfile.TarInfo, dict, dict | None]] = {}
        for rel in CHANGED_PATHS:
            entry = entries.get(rel)
            if entry is None or entry.get("type") != "file" or entry.get("mode") != "0o644":
                raise StageError("final manifest does not admit a 0644 regular file: " + rel)
            member = by_path[rel]
            if member.size != int(entry["bytes"]):
                raise StageError("archive member size differs from manifest: " + rel)
            relative = safe_rel(rel)
            target = _ensure_real_parents(source_root, relative)
            before = base_entries.get(rel)
            if rel == NEW_PATH:
                if before is not None:
                    raise StageError("explicit new path exists in the base manifest")
                try:
                    target.lstat()
                except FileNotFoundError:
                    pass
                else:
                    raise StageError("new path already exists in the private clone")
            else:
                if before is None or before.get("type") != "file":
                    raise StageError("replacement path lacks a base regular-file entry: " + rel)
                try:
                    current = target.lstat()
                except FileNotFoundError as exc:
                    raise StageError("replacement target is missing: " + rel) from exc
                if not stat.S_ISREG(current.st_mode):
                    raise StageError("replacement target is not a regular file: " + rel)
                if (
                    current.st_size != int(before["bytes"])
                    or sha256_file(target) != before["sha256"]
                    or stat.S_IMODE(current.st_mode) != int(before["mode"], 8)
                ):
                    raise StageError("replacement target differs from the verified base: " + rel)
            targets[rel] = (target, member, entry, before)

        for index, rel in enumerate(CHANGED_PATHS):
            target, member, entry, before = targets[rel]
            stream = tar.extractfile(member)
            if stream is None:
                raise StageError("cannot read archive member: " + rel)
            digest = hashlib.sha256()
            count = 0
            if rel == NEW_PATH:
                temp = target
                open_mode = "xb"
            else:
                temp = target.parent / (".overlay-" + str(os.getpid()) + "-" + str(index))
                if temp.exists() or temp.is_symlink():
                    stream.close()
                    raise StageError("overlay temporary path already exists")
                open_mode = "xb"
            try:
                with temp.open(open_mode) as output:
                    while True:
                        block = stream.read(1024 * 1024)
                        if not block:
                            break
                        count += len(block)
                        if count > int(entry["bytes"]):
                            raise StageError("archive member exceeds its pinned size: " + rel)
                        digest.update(block)
                        output.write(block)
                    output.flush()
                    os.fsync(output.fileno())
                if count != int(entry["bytes"]) or digest.hexdigest() != entry["sha256"]:
                    raise StageError("archive member hash/size differs from final manifest: " + rel)
                os.chmod(temp, 0o644)
                if rel != NEW_PATH:
                    os.replace(temp, target)
            except BaseException:
                try:
                    temp.unlink()
                except FileNotFoundError:
                    pass
                raise
            finally:
                stream.close()


def run_verifier(verifier: pathlib.Path, source: pathlib.Path, manifest: pathlib.Path,
                 kind: str, base_manifest: pathlib.Path, compare_to: pathlib.Path | None = None) -> dict:
    command = [sys.executable, str(verifier), "--source", str(source), "--manifest", str(manifest),
               "--manifest-kind", kind, "--base-manifest", str(base_manifest)]
    if compare_to is not None:
        command.extend(["--compare-inodes-to", str(compare_to)])
    completed = subprocess.run(command, check=False, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if completed.returncode != 0:
        raise StageError("source manifest verifier failed: " + completed.stderr.strip())
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise StageError("source verifier returned invalid JSON") from exc


def write_exclusive(path: pathlib.Path, data: bytes) -> None:
    with path.open("xb") as stream:
        stream.write(data)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(path, 0o600)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--packet", required=True, type=pathlib.Path)
    parser.add_argument("--base-source", required=True, type=pathlib.Path)
    parser.add_argument("--release-parent", required=True, type=pathlib.Path)
    parser.add_argument("--verifier", type=pathlib.Path, default=HERE / "verify_source_manifest.py")
    args = parser.parse_args()
    packet = args.packet
    base_source = args.base_source
    release_parent = args.release_parent
    expected_packet = pathlib.Path.home() / ".local/share/nudox/validation-inputs" / COMMIT
    expected_base = pathlib.Path.home() / ".local/share/nudox/source-releases" / BASE_COMMIT / "source"
    expected_release_parent = pathlib.Path.home() / ".local/share/nudox/source-releases"
    if packet != expected_packet or base_source != expected_base or release_parent != expected_release_parent:
        raise StageError("packet/base/release paths differ from the pinned f85-to-40ab deployment paths")
    check_owned_directory(packet, require_private=True)
    check_owned_directory(base_source, require_private=False)
    check_home_descendants(base_source)
    check_owned_directory(release_parent, require_private=False)
    check_home_descendants(release_parent)
    if args.verifier.is_symlink() or not args.verifier.is_file() or sha256_file(args.verifier) != VERIFIER_SHA:
        raise StageError("verifier is missing, symlinked, or differs from its exact pinned hash")

    base_manifest_path = packet / "base-source-verification.json"
    final_manifest_path = packet / "source-verification.json"
    candidate_path = packet / "candidate-receipt.json"
    package_path = packet / "package-receipt.json"
    archive_path = packet / "source-delta.tar.gz"
    patch_path = packet / "source-delta.patch"
    if sha256_file(base_manifest_path) != BASE_MANIFEST_SHA:
        raise StageError("base manifest hash mismatch")
    if sha256_file(final_manifest_path) != FINAL_MANIFEST_SHA:
        raise StageError("final manifest hash mismatch")
    if sha256_file(candidate_path) != CANDIDATE_SHA:
        raise StageError("candidate receipt hash mismatch")
    if sha256_file(archive_path) != DELTA_SHA or archive_path.stat().st_size != DELTA_BYTES:
        raise StageError("delta archive hash/size mismatch")
    if sha256_file(patch_path) != SOURCE_PATCH_SHA:
        raise StageError("source delta patch hash mismatch")
    package_bytes = package_path.read_bytes()
    if sha256_bytes(package_bytes) != PACKET_SHA:
        raise StageError("package receipt hash mismatch")

    base = load_json(base_manifest_path)
    final = load_json(final_manifest_path)
    candidate = load_json(candidate_path)
    package = load_json(package_path)
    validate_manifest_delta(base, final, candidate, package)
    base_entries = manifest_entries(base)
    final_entries = manifest_entries(final)
    verifier_base = run_verifier(args.verifier, base_source, base_manifest_path, "base", base_manifest_path)
    if verifier_base.get("entries") != BASE_ENTRIES:
        raise StageError("f85 base source entry count mismatch")

    release = release_parent / COMMIT
    if release.exists() or release.is_symlink():
        raise StageError("candidate source release already exists: " + str(release))
    staging = release_parent / ("." + COMMIT + ".staging-" + str(os.getpid()) + "-" + str(int(time.time())))
    if staging.exists() or staging.is_symlink():
        raise StageError("staging path already exists: " + str(staging))
    staging.mkdir(mode=0o700)
    os.chmod(staging, 0o700)
    published = False
    try:
        source_copy = staging / "source"
        receipt_dir = staging / "receipt"
        receipt_dir.mkdir(mode=0o700)
        os.chmod(receipt_dir, 0o700)
        check_owned_directory(staging, require_private=True)
        check_owned_directory(receipt_dir, require_private=True)
        clone = subprocess.run(
            ["/bin/cp", "-cRp", str(base_source), str(source_copy)],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if clone.returncode != 0:
            raise StageError("APFS clone failed; no fallback copy attempted: " + clone.stderr.strip())
        cloned_base = run_verifier(
            args.verifier, source_copy, base_manifest_path, "base", base_manifest_path, base_source
        )
        if cloned_base.get("entries") != BASE_ENTRIES:
            raise StageError("cloned f85 source failed base verification")
        overlay_files(
            archive_path,
            source_copy,
            list(CHANGED_PATHS),
            final_entries,
            base_entries,
        )
        final_result = run_verifier(
            args.verifier,
            source_copy,
            final_manifest_path,
            "final",
            base_manifest_path,
            base_source,
        )
        base_after = run_verifier(args.verifier, base_source, base_manifest_path, "base", base_manifest_path)
        if final_result.get("entries") != FINAL_ENTRIES or base_after.get("entries") != BASE_ENTRIES:
            raise StageError("final/base source entry counts differ from the pinned packet")

        for source, name in (
            (base_manifest_path, "base-source-verification.json"),
            (final_manifest_path, "source-verification.json"),
            (candidate_path, "candidate-receipt.json"),
            (package_path, "package-receipt.json"),
            (archive_path, "source-delta.tar.gz"),
            (patch_path, "source-delta.patch"),
            (args.verifier, "verify_source_manifest.py"),
            (HERE / pathlib.Path(__file__).name, "prepare_candidate.py"),
        ):
            if source.is_symlink() or not source.is_file():
                raise StageError("receipt input is not a regular file: " + str(source))
            write_exclusive(receipt_dir / name, source.read_bytes())
        helper_hashes = {
            "prepare_candidate.py": sha256_file(HERE / "prepare_candidate.py"),
            "verify_source_manifest.py": sha256_file(args.verifier),
        }
        staging_receipt = {
            "status": "source_staged_verified",
            "source_commit": COMMIT,
            "source_tree": TREE,
            "base_commit": BASE_COMMIT,
            "base_manifest_sha256": BASE_MANIFEST_SHA,
            "candidate_manifest_sha256": FINAL_MANIFEST_SHA,
            "candidate_receipt_sha256": CANDIDATE_SHA,
            "source_receipt_sha256": SOURCE_RECEIPT_SHA,
            "package_receipt_sha256": PACKET_SHA,
            "delta_sha256": DELTA_SHA,
            "delta_bytes": DELTA_BYTES,
            "source_delta_patch_sha256": SOURCE_PATCH_SHA,
            "cargo_lock_sha256": LOCK_SHA,
            "gitlink_commit": GITLINK_COMMIT,
            "changed_paths": list(CHANGED_PATHS),
            "entry_count": FINAL_ENTRIES,
            "parent_entries": PARENT_FINAL_ENTRIES,
            "gitlink_entries": GITLINK_ENTRIES,
            "base_verified": verifier_base,
            "clone_verified_without_shared_inodes": cloned_base,
            "final_verified_without_shared_inodes": final_result,
            "base_reverified_after_overlay": base_after,
            "helper_sha256": helper_hashes,
            "no_cargo_or_remote_service_action": True,
        }
        write_exclusive(
            receipt_dir / "staging-result.json",
            (json.dumps(staging_receipt, sort_keys=True, indent=2) + "\n").encode(),
        )
        if release.exists() or release.is_symlink():
            raise StageError("candidate release path appeared during materialization")
        rename_exclusive(staging, release)
        published = True
    finally:
        if not published and staging.exists() and not staging.is_symlink():
            shutil.rmtree(staging)
    print(json.dumps({
        "status": "source_staged_verified",
        "release": str(release),
        "source": str(release / "source"),
        "receipt": str(release / "receipt" / "staging-result.json"),
        "entries": FINAL_ENTRIES,
        "commit": COMMIT,
        "tree": TREE,
    }, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (StageError, OSError, tarfile.TarError) as exc:
        print("STAGE_FAILED: " + str(exc), file=sys.stderr)
        raise SystemExit(2)
