#!/usr/bin/env python3
"""Local protocol tests for the pinned borrowed-candidate source packet.

These tests exercise packet/overlay validation only. They do not claim Rust,
Cargo, Nix, APFS-clone, or full source-materialization acceptance.
"""
from __future__ import annotations

import copy
import hashlib
import importlib.util
import io
import json
import pathlib
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
HELPERS = ROOT / "helpers"
PACKET = ROOT / "packet"
REPOSITORY = pathlib.Path("/private/tmp/nudox-canonical-integration-20261004")
sys.path.insert(0, str(HELPERS))

import prepare_candidate as stager  # noqa: E402
import verify_source_manifest as verifier  # noqa: E402


def load_packet() -> tuple[dict, dict, dict, dict]:
    base = json.loads((PACKET / "base-source-verification.json").read_text())
    final = json.loads((PACKET / "source-verification.json").read_text())
    candidate = json.loads((PACKET / "candidate-receipt.json").read_text())
    package = json.loads((PACKET / "package-receipt.json").read_text())
    return base, final, candidate, package


def make_archive(tmp: pathlib.Path, members: list[tuple[str, int, bytes, str | None]]) -> pathlib.Path:
    archive_path = tmp / "hostile.tar.gz"
    with tarfile.open(archive_path, mode="w:gz", format=tarfile.PAX_FORMAT) as archive:
        for name, kind, payload, linkname in members:
            info = tarfile.TarInfo(name)
            info.type = kind
            info.mode = 0o644
            info.size = len(payload) if kind == tarfile.REGTYPE else 0
            if linkname is not None:
                info.linkname = linkname
            archive.addfile(info, io.BytesIO(payload) if kind == tarfile.REGTYPE else None)
    return archive_path


class CandidatePacketTests(unittest.TestCase):
    def test_positive_packet_and_private_overlay_protocol(self) -> None:
        base, final, candidate, package = load_packet()
        verifier.validate_manifest_pair(base, final)
        stager.validate_manifest_delta(base, final, candidate, package)

        delta = PACKET / "source-delta.tar.gz"
        self.assertEqual(verifier.sha256_file(delta), verifier.DELTA_SHA)
        self.assertEqual(delta.stat().st_size, verifier.DELTA_BYTES)
        with tarfile.open(delta, mode="r:gz") as archive:
            members = archive.getmembers()
            self.assertEqual(
                [member.name for member in members],
                ["source/" + path for path in stager.CHANGED_PATHS],
            )
            self.assertTrue(all(member.isfile() and member.mode == 0o644 for member in members))

        # Use only the nine changed targets in a disposable fixture. This tests
        # overlay admission and bytes; it is not a whole-tree verification.
        base_rows = {row["path"]: row for row in base["source"]["files"]}
        final_rows = {row["path"]: row for row in final["source"]["files"]}
        with tempfile.TemporaryDirectory(prefix="borrowed-candidate-overlay-") as temp:
            source = pathlib.Path(temp) / "source"
            source.mkdir()
            for path in stager.REPLACED_PATHS:
                target = source.joinpath(*pathlib.PurePosixPath(path).parts)
                target.parent.mkdir(parents=True, exist_ok=True)
                data = subprocess.run(
                    ["git", "-C", str(REPOSITORY), "cat-file", "blob", f"{stager.BASE_COMMIT}:{path}"],
                    check=True,
                    stdout=subprocess.PIPE,
                ).stdout
                self.assertEqual(hashlib.sha256(data).hexdigest(), base_rows[path]["sha256"])
                target.write_bytes(data)
                target.chmod(0o644)
            new_target = source.joinpath(*pathlib.PurePosixPath(stager.NEW_PATH).parts)
            new_target.parent.mkdir(parents=True, exist_ok=True)

            stager.overlay_files(
                delta,
                source,
                list(stager.CHANGED_PATHS),
                stager.manifest_entries(final),
                stager.manifest_entries(base),
            )
            self.assertTrue(source.joinpath(*pathlib.PurePosixPath(stager.NEW_PATH).parts).is_file())
            for path in stager.CHANGED_PATHS:
                materialized = source.joinpath(*pathlib.PurePosixPath(path).parts)
                self.assertEqual(hashlib.sha256(materialized.read_bytes()).hexdigest(), final_rows[path]["sha256"])
                self.assertEqual(materialized.stat().st_mode & 0o777, 0o644)

    def test_unknown_new_path_is_rejected(self) -> None:
        base, final, candidate, package = load_packet()
        mutated = copy.deepcopy(final)
        row = next(row for row in mutated["source"]["files"] if row["path"] == stager.NEW_PATH)
        row["path"] = "crates/library/tests/unreviewed_source_file.rs"
        with self.assertRaises(verifier.VerificationError):
            verifier.validate_manifest_pair(base, mutated)
        with self.assertRaises(stager.StageError):
            stager.validate_manifest_delta(base, mutated, candidate, package)

    def test_third_source_change_is_rejected(self) -> None:
        base, final, _, _ = load_packet()
        mutated = copy.deepcopy(final)
        changed = set(verifier.CHANGED_PATHS)
        row = next(row for row in mutated["source"]["files"] if row["path"] not in changed)
        row["sha256"] = "0" * 64
        with self.assertRaisesRegex(verifier.VerificationError, "source changed paths"):
            verifier.validate_manifest_pair(base, mutated)

    def test_lock_and_gitlink_mismatches_are_rejected(self) -> None:
        base, final, _, _ = load_packet()
        wrong_lock = copy.deepcopy(final)
        wrong_lock["source"]["cargo_lock_sha256"] = "0" * 64
        with self.assertRaisesRegex(verifier.VerificationError, "Cargo.lock"):
            verifier.validate_manifest_pair(base, wrong_lock)

        wrong_link = copy.deepcopy(final)
        wrong_link["gitlink"]["commit"] = "1" * 40
        with self.assertRaisesRegex(verifier.VerificationError, "gitlink"):
            verifier.validate_manifest_pair(base, wrong_link)

    def assert_archive_rejected(self, archive: pathlib.Path, expected: str) -> None:
        old_sha, old_bytes = stager.DELTA_SHA, stager.DELTA_BYTES
        try:
            stager.DELTA_SHA = verifier.sha256_file(archive)
            stager.DELTA_BYTES = archive.stat().st_size
            with tempfile.TemporaryDirectory(prefix="borrowed-candidate-hostile-") as temp:
                source = pathlib.Path(temp) / "source"
                source.mkdir()
                with self.assertRaisesRegex(stager.StageError, expected):
                    stager.overlay_files(
                        archive,
                        source,
                        list(stager.CHANGED_PATHS),
                        {},
                        {},
                    )
        finally:
            stager.DELTA_SHA, stager.DELTA_BYTES = old_sha, old_bytes

    def test_archive_path_traversal_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="borrowed-candidate-path-") as temp:
            root = pathlib.Path(temp)
            archive = make_archive(root, [("source/../../escape", tarfile.REGTYPE, b"x", None)])
            self.assert_archive_rejected(archive, "unsafe or non-canonical path")
            self.assertFalse((root.parent / "escape").exists())

    def test_unknown_archive_path_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="borrowed-candidate-unknown-") as temp:
            archive = make_archive(
                pathlib.Path(temp),
                [("source/crates/library/tests/not-allowlisted.rs", tarfile.REGTYPE, b"x", None)],
            )
            self.assert_archive_rejected(archive, "unknown archive path")

    def test_duplicate_archive_path_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="borrowed-candidate-duplicate-") as temp:
            member = ("source/" + stager.CHANGED_PATHS[0], tarfile.REGTYPE, b"x", None)
            archive = make_archive(pathlib.Path(temp), [member, member])
            self.assert_archive_rejected(archive, "duplicate archive member")

    def test_archive_symlink_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory(prefix="borrowed-candidate-symlink-") as temp:
            archive = make_archive(
                pathlib.Path(temp),
                [("source/" + stager.CHANGED_PATHS[0], tarfile.SYMTYPE, b"", "/tmp/target")],
            )
            self.assert_archive_rejected(archive, "not a 0644 regular file")


if __name__ == "__main__":
    unittest.main(verbosity=2)
