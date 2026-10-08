"""Data and filesystem regressions for the Linux installer; no product ELF is faked."""
import hashlib
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import install_linux as installer


class InstallerMetadataTests(unittest.TestCase):
    def setUp(self):
        self.old_tag = installer.PINNED_RELEASE_TAG
        self.old_hash = installer.PINNED_MANIFEST_SHA256

    def tearDown(self):
        installer.PINNED_RELEASE_TAG = self.old_tag
        installer.PINNED_MANIFEST_SHA256 = self.old_hash

    def test_pinned_manifest_requires_exact_bytes_and_source_bound_tag(self):
        source = "a" * 40
        manifest = {
            "schema": 1,
            "version": "0.2.0",
            "source_sha": source,
            "source_tree": "b" * 40,
            "cargo_lock_sha256": "c" * 64,
            "platform": "linux-x64",
            "target": "x86_64-unknown-linux-gnu",
            "asset": f"nudox-linux-x86_64-{source[:10]}.tar.gz",
            "sha256": "d" * 64,
            "size_bytes": 123,
            "minimum_glibc": "2.35",
            "build_manifest_sha256": "e" * 64,
            "packaging_manifest_sha256": "f" * 64,
        }
        raw = json.dumps(manifest, sort_keys=True).encode()
        installer.PINNED_RELEASE_TAG = f"checkpoint-20261006-{source[:10]}-linux-x64"
        installer.PINNED_MANIFEST_SHA256 = hashlib.sha256(raw).hexdigest()

        entry, parsed = installer.parse_pinned_manifest(installer.PINNED_RELEASE_TAG, raw)

        self.assertEqual(parsed, manifest)
        self.assertEqual(entry["tag"], installer.PINNED_RELEASE_TAG)
        with self.assertRaisesRegex(installer.InstallError, "manifest SHA-256 mismatch"):
            installer.parse_pinned_manifest(installer.PINNED_RELEASE_TAG, raw + b" ")

    def test_local_candidate_install_is_unavailable_in_unpinned_channel_script(self):
        with tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(installer.InstallError, "only in a digest-pinned"):
                installer.load_local_candidate(Path(directory), Path(directory))


class InstallerFilesystemTests(unittest.TestCase):
    def test_reuse_marker_refuses_links_fifos_and_oversize_without_runtime_execution(self):
        for kind in ("symlink", "fifo", "oversize"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                marker = root / ".installed-release.json"
                if kind == "symlink":
                    outside = root / "other-marker"
                    outside.write_text("{}")
                    os.symlink(outside, marker)
                elif kind == "fifo":
                    os.mkfifo(marker)
                else:
                    marker.write_bytes(b" " * (16 * 1024 + 1))
                with patch.object(installer, "verify_package") as verify:
                    with self.assertRaises(installer.InstallError):
                        installer.verify_existing_install(root, {}, {"tag":"fixture"}, {})
                    verify.assert_not_called()

    def test_existing_package_aliases_are_refused_even_when_bytes_are_unchanged(self):
        for relative, directory in (("share", True), ("share/nudox/typescript", True),
                                    ("lib", True), ("lib/libgcc_s.so.1", False)):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as temporary:
                base = Path(temporary).resolve()
                root = base / "package"
                (root / "share/nudox/typescript").mkdir(parents=True)
                (root / "lib").mkdir()
                (root / "lib/libgcc_s.so.1").write_bytes(b"unchanged installed bytes")
                original = root / relative
                outside = base / "outside"
                original.rename(outside)
                os.symlink(outside, original)
                with self.assertRaisesRegex(installer.InstallError, "link or invalid"):
                    installer._verified_package_path(root, relative, directory=directory)
                # Descendants cannot make a linked parent acceptable either.
                if directory:
                    with self.assertRaisesRegex(installer.InstallError, "link or invalid"):
                        installer._verified_package_path(root, relative + "/member")

    def test_existing_marker_does_not_skip_rechecking_installed_files(self):
        marker = {"version": "0.2.0", "tag": "checkpoint-test", "source_sha": "a" * 40, "asset": "release.tar.gz", "sha256": "b" * 64}
        entry = {"tag": marker["tag"]}
        manifest = {"sha256": marker["sha256"]}
        with tempfile.TemporaryDirectory() as directory:
            final = Path(directory).resolve()
            (final / ".installed-release.json").write_text(json.dumps(marker))
            with patch.object(installer, "verify_package", side_effect=installer.InstallError("executable hash mismatch")) as verify:
                with self.assertRaisesRegex(installer.InstallError, "failed its integrity/startup recheck"):
                    installer.verify_existing_install(final, marker, entry, manifest)
            verify.assert_called_once_with(final, entry, manifest)

    def test_marker_mismatch_fails_before_runtime_recheck(self):
        expected = {"tag": "checkpoint-test", "sha256": "a" * 64}
        entry = {"tag": "checkpoint-test"}
        with tempfile.TemporaryDirectory() as directory:
            final = Path(directory).resolve()
            (final / ".installed-release.json").write_text(json.dumps({"tag": "checkpoint-test", "sha256": "b" * 64}))
            with patch.object(installer, "verify_package") as verify:
                with self.assertRaisesRegex(installer.InstallError, "different bytes"):
                    installer.verify_existing_install(final, expected, entry, {})
            verify.assert_not_called()

    def test_current_mcp_command_names_resolve_to_sibling_install_paths(self):
        managed_root = Path("/tmp/private-prefix/lib/nudox")
        links = installer.command_links(managed_root)
        for name in ("backend-mcp", "nudox-mcp"):
            self.assertEqual(links[name], str(managed_root / "current/bin/backend-mcp"))
        for name in ("backend-locald", "nudox-locald"):
            self.assertEqual(links[name], str(managed_root / "current/bin/backend-locald"))

    def test_tar_path_and_link_metadata_are_rejected_without_extracting_binaries(self):
        traversal = tarfile.TarInfo("nudox-linux-x86_64/../../outside")
        with self.assertRaisesRegex(installer.InstallError, "unsafe path"):
            installer._safe_member(traversal, "nudox-linux-x86_64")

        link = tarfile.TarInfo("nudox-linux-x86_64/bin/backend-mcp")
        link.type = tarfile.SYMTYPE
        link.linkname = "../../outside"
        with self.assertRaisesRegex(installer.InstallError, "link or special file"):
            installer._safe_member(link, "nudox-linux-x86_64")


if __name__ == "__main__":
    unittest.main()
