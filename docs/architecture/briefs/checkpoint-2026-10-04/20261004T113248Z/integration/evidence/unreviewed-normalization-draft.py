#!/usr/bin/env python3
"""APFS-clone source13, correct only its recorded extraction-mode drift, verify."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import stat
import subprocess
import sys
import time

PYTHON = "/nix/store/llk2h8rxqzv7zh53bi413ffibjrxskxw-python3-3.14.6/bin/python3.14"
PYTHON_SHA = "4f3c8b2a70cc7967e1ce900871443300357d017c1c67dfcca74bfe3e9bb6c8ad"
MANIFEST_SHA = "ca834ddcdf16990d59fd040476fecfa3591b20161f3ae527dfd6b7737baea7a2"
VERIFIER_SHA = "981c7313c8ea071d44bd58800c834b0b23c459c8c4c4fa6e6f89486129ab9acb"
COMMIT = "13d9919ae6a9fc4d662c5ffb322462c6e3dfb25d"
TREE = "e8a0ba54bdfae0698aca136b9400d9a1c8a89817"
DRIFT_COUNT = 4108
O_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
O_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)


class Refusal(RuntimeError):
    pass


def sha(path: pathlib.Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def digest(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def check_dir(path: pathlib.Path, private: bool = False) -> None:
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or info.st_uid != os.getuid():
        raise Refusal("directory missing, symlinked, or not user-owned: " + str(path))
    mode = stat.S_IMODE(info.st_mode)
    if mode & 0o022 or (private and mode != 0o700):
        raise Refusal("directory permissions are not approved: " + str(path))


def inventory(root: pathlib.Path, verifier) -> dict:
    if root.is_symlink() or not root.is_dir():
        raise Refusal("source tree missing or symlinked: " + str(root))
    return verifier.walk_payload(root)


def fields(rows: dict) -> list:
    result = []
    for rel in sorted(rows):
        kind, mode, size, content_sha, st = rows[rel]
        result.append([rel, kind, mode, size, content_sha, st.st_dev, st.st_ino, st.st_nlink])
    return result


def baseline(rows: dict, expected: dict, gitlink_paths: set[str]) -> list:
    if set(rows) != set(expected):
        raise Refusal("source paths differ from pinned manifest")
    drift = []
    inodes = set()
    for rel in sorted(expected):
        row, observed = expected[rel], rows[rel]
        kind, mode, size, content_sha, st = observed
        expected_mode = int(row["mode"], 8) if isinstance(row["mode"], str) else int(row["mode"])
        if kind != row["type"] or size != int(row["bytes"]) or content_sha != row["sha256"]:
            raise Refusal("source content/type differs from pinned manifest: " + rel)
        if kind == "file":
            if st.st_nlink != 1 or (st.st_dev, st.st_ino) in inodes:
                raise Refusal("source regular-file identity is not unique: " + rel)
            inodes.add((st.st_dev, st.st_ino))
        actual_mode = int(mode, 8)
        if actual_mode == expected_mode:
            continue
        if rel not in gitlink_paths or not (
            (kind == "file" and expected_mode == 0o664 and actual_mode == 0o644) or
            (kind == "symlink" and expected_mode == 0o777 and actual_mode == 0o755)
        ):
            raise Refusal("mode drift differs from the proven archive normalization: " + rel)
        drift.append([rel, kind, actual_mode, expected_mode, content_sha])
    if len(drift) != DRIFT_COUNT:
        raise Refusal("expected exactly 4108 gitlink mode-only differences")
    return drift


def clone_relation(before: dict, after: dict, same_modes: bool) -> dict:
    if set(before) != set(after):
        raise Refusal("clone path set differs from source")
    regular = 0
    for rel in sorted(before):
        a, b = before[rel], after[rel]
        if a[:1] + a[2:4] != b[:1] + b[2:4]:
            raise Refusal("clone type/size/hash differs from source: " + rel)
        if same_modes and a[1] != b[1]:
            raise Refusal("APFS clone did not preserve original mode: " + rel)
        if b[0] == "file":
            regular += 1
            if b[4].st_nlink != 1 or (a[4].st_dev, a[4].st_ino) == (b[4].st_dev, b[4].st_ino):
                raise Refusal("clone regular-file inode/link identity is unsafe: " + rel)
    return {"entries": len(before), "regular_files": regular,
            "source_inventory_sha256": digest(fields(before)),
            "clone_inventory_sha256": digest(fields(after)),
            "regular_file_inodes_disjoint": True}


def parent_fd(root_fd: int, parts: tuple[str, ...]) -> tuple[int, str]:
    fd = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            next_fd = os.open(part, os.O_RDONLY | O_DIRECTORY | O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = next_fd
        return fd, parts[-1]
    except BaseException:
        os.close(fd)
        raise


def set_mode_nofollow(root_fd: int, rel: str, observed, expected_mode: int) -> None:
    fd, name = parent_fd(root_fd, pathlib.PurePosixPath(rel).parts)
    try:
        before = os.stat(name, dir_fd=fd, follow_symlinks=False)
        old = observed[4]
        if (before.st_dev, before.st_ino, stat.S_IMODE(before.st_mode)) != (
            old.st_dev, old.st_ino, int(observed[1], 8)
        ):
            raise Refusal("clone entry changed before no-follow mode update: " + rel)
        if observed[0] == "file":
            file_fd = os.open(name, os.O_RDONLY | O_NOFOLLOW, dir_fd=fd)
            try:
                opened = os.fstat(file_fd)
                if not stat.S_ISREG(opened.st_mode) or (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino) or opened.st_nlink != 1:
                    raise Refusal("regular-file no-follow identity check failed: " + rel)
                os.fchmod(file_fd, expected_mode)
                updated = os.fstat(file_fd)
                if (updated.st_dev, updated.st_ino, stat.S_IMODE(updated.st_mode)) != (
                    opened.st_dev, opened.st_ino, expected_mode
                ):
                    raise Refusal("regular-file fchmod verification failed: " + rel)
            finally:
                os.close(file_fd)
        else:
            if os.chmod not in os.supports_dir_fd or os.chmod not in os.supports_follow_symlinks:
                raise Refusal("no safe dirfd/no-follow symlink chmod; refusing fallback")
            os.chmod(name, expected_mode, dir_fd=fd, follow_symlinks=False)
            updated = os.stat(name, dir_fd=fd, follow_symlinks=False)
            if not stat.S_ISLNK(updated.st_mode) or (updated.st_dev, updated.st_ino) != (before.st_dev, before.st_ino) or stat.S_IMODE(updated.st_mode) != expected_mode:
                raise Refusal("symlink no-follow chmod identity/mode check failed: " + rel)
    finally:
        os.close(fd)


def write_receipt(path: pathlib.Path, receipt: dict) -> None:
    check_dir(path.parent, private=True)
    fd = os.open(str(path), os.O_WRONLY | os.O_CREAT | os.O_EXCL | O_NOFOLLOW, 0o600)
    try:
        payload = (json.dumps(receipt, sort_keys=True, indent=2) + "\n").encode()
        while payload:
            count = os.write(fd, payload)
            payload = payload[count:]
        os.fsync(fd)
    finally:
        os.close(fd)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", required=True, type=pathlib.Path)
    parser.add_argument("--base-container", required=True, type=pathlib.Path)
    parser.add_argument("--manifest", required=True, type=pathlib.Path)
    parser.add_argument("--verifier", required=True, type=pathlib.Path)
    parser.add_argument("--receipt", required=True, type=pathlib.Path)
    args = parser.parse_args()
    stage = "preflight"
    receipt = {"status": "failed", "stage": stage, "started_utc_epoch": int(time.time())}
    try:
        python_path = pathlib.Path(PYTHON)
        if python_path.is_symlink() or not python_path.is_file() or os.path.realpath(sys.executable) != PYTHON or sha(python_path) != PYTHON_SHA or sys.version.split()[0] != "3.14.6":
            raise Refusal("pinned Python executable/hash/version mismatch")
        for path in (args.source.parent, args.base_container.parent, args.manifest.parent, args.verifier.parent, args.receipt.parent):
            check_dir(path, private=True)
        check_dir(args.source, private=True)
        if args.source.is_symlink() or not args.source.is_dir() or not (args.source / "Cargo.lock").is_file() or (args.source / "Cargo.lock").is_symlink():
            raise Refusal("installed source or Cargo.lock missing/symlinked")
        if args.manifest.is_symlink() or not args.manifest.is_file() or args.verifier.is_symlink() or not args.verifier.is_file():
            raise Refusal("manifest or unchanged verifier missing/symlinked")
        if args.receipt.exists() or args.receipt.is_symlink() or args.base_container.exists() or args.base_container.is_symlink():
            raise Refusal("refusing existing output or receipt path")
        if sha(args.verifier) != VERIFIER_SHA:
            raise Refusal("unchanged source verifier SHA-256 mismatch")
        if sha(args.source / "Cargo.lock") != "5f997f548c3f47161dd044b441381936a51cb3db35d7ae699c46473769d5467f":
            raise Refusal("installed Cargo.lock hash mismatch")
        sys.path.insert(0, str(args.verifier.parent))
        import verify_source_manifest as verifier
        if sha(args.manifest) != MANIFEST_SHA:
            raise Refusal("base manifest SHA-256 mismatch")
        manifest = verifier.load_manifest(args.manifest, MANIFEST_SHA, "base")
        entries = verifier.entries_by_path(manifest)
        gitlink_paths = {row["path"] for row in manifest["gitlink"]["files"]}

        stage = "source_inventory"
        source_before = inventory(args.source, verifier)
        source_drift = baseline(source_before, entries, gitlink_paths)
        stage = "exclusive_private_container"
        os.mkdir(args.base_container, 0o700)
        check_dir(args.base_container, private=True)
        clone = args.base_container / "source"
        command = ["/bin/cp", "-cRp", str(args.source), str(clone)]
        stage = "apfs_clone"
        copied = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=False)
        receipt.update({"copy_command": command, "copy_exit": copied.returncode,
                       "copy_stderr_sha256": hashlib.sha256(copied.stderr.encode()).hexdigest()})
        if copied.returncode != 0:
            raise Refusal("APFS clone failed; no fallback attempted: " + copied.stderr.strip())
        if clone.is_symlink() or not clone.is_dir():
            raise Refusal("APFS clone output is missing or symlinked")
        os.chmod(clone, 0o700)
        clone_before = inventory(clone, verifier)
        before_relation = clone_relation(source_before, clone_before, same_modes=True)
        if baseline(clone_before, entries, gitlink_paths) != source_drift:
            raise Refusal("clone mode drift does not match original source evidence")

        stage = "clone_mode_normalization"
        root_fd = os.open(str(clone), os.O_RDONLY | O_DIRECTORY | O_NOFOLLOW)
        changes = []
        try:
            for rel in sorted(entries):
                observed = clone_before[rel]
                row = entries[rel]
                wanted = int(row["mode"], 8) if isinstance(row["mode"], str) else int(row["mode"])
                if int(observed[1], 8) != wanted:
                    set_mode_nofollow(root_fd, rel, observed, wanted)
                    changes.append([rel, observed[0], int(observed[1], 8), wanted, observed[3]])
        finally:
            os.close(root_fd)
        if len(changes) != DRIFT_COUNT:
            raise Refusal("normalizer did not apply exactly the proven mode changes")

        stage = "clone_postcheck"
        clone_after = inventory(clone, verifier)
        after_relation = clone_relation(source_before, clone_after, same_modes=False)
        full = verifier.verify_tree(clone, manifest, args.source)
        command = [PYTHON, str(args.verifier), "--source", str(clone), "--manifest", str(args.manifest),
                   "--manifest-kind", "base", "--compare-inodes-to", str(args.source)]
        checked = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=False)
        receipt.update({"verifier_command": command, "verifier_exit": checked.returncode,
                       "verifier_stdout_sha256": hashlib.sha256(checked.stdout.encode()).hexdigest(),
                       "verifier_stderr_sha256": hashlib.sha256(checked.stderr.encode()).hexdigest()})
        if checked.returncode != 0:
            raise Refusal("unchanged full source verifier failed: " + checked.stderr.strip())
        stage = "source_unchanged_postcheck"
        source_after = inventory(args.source, verifier)
        if fields(source_before) != fields(source_after):
            raise Refusal("installed source metadata/content/identity changed during normalization")
        receipt.update({
            "status": "normalized_base_verified", "stage": "complete",
            "python": {"path": PYTHON, "version": "3.14.6", "sha256": PYTHON_SHA},
            "base_commit": COMMIT, "base_tree": TREE, "manifest_sha256": MANIFEST_SHA,
            "verifier_sha256": VERIFIER_SHA,
            "source_before": {"entries": len(source_before), "inventory_sha256": digest(fields(source_before)),
                              "mode_drift_count": len(source_drift), "mode_drift_rows_sha256": digest(source_drift)},
            "clone_before": {"inventory_sha256": digest(fields(clone_before)), "source_relation": before_relation},
            "clone_after": {"entries": len(clone_after), "inventory_sha256": digest(fields(clone_after)),
                            "source_relation": after_relation, "full_verifier": full,
                            "standalone_verifier_stdout": json.loads(checked.stdout)},
            "mode_changes": {"count": len(changes), "rows_sha256": digest(changes),
                             "regular_files": sum(row[1] == "file" for row in changes),
                             "symlinks": sum(row[1] == "symlink" for row in changes)},
            "source_unchanged_after_check": True, "completed_utc_epoch": int(time.time())})
        write_receipt(args.receipt, receipt)
        print(json.dumps({"status": receipt["status"], "source": str(clone), "receipt": str(args.receipt),
                          "entries": len(clone_after), "mode_changes": len(changes), "verifier_exit": checked.returncode}, sort_keys=True))
        return 0
    except Exception as exc:
        receipt.update({"status": "failed", "stage": stage, "error_type": type(exc).__name__,
                        "error": str(exc), "completed_utc_epoch": int(time.time())})
        try:
            if not args.receipt.exists() and not args.receipt.is_symlink():
                write_receipt(args.receipt, receipt)
        except Exception as receipt_error:
            print("NORMALIZE_RECEIPT_FAILED: " + str(receipt_error), file=sys.stderr)
        print("NORMALIZE_FAILED: " + str(exc), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
