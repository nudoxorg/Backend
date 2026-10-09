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
    @staticmethod
    def _sdk_fixture(root):
        contents = {
            "share/nudox/typescript/node/bin/node": b"fixture node executable",
            "share/nudox/typescript/node/LICENSE": b"node license",
            "share/nudox/typescript/node_modules/typescript/package.json": b'{"version":"5.9.3"}',
            "share/nudox/typescript/node_modules/typescript/bin/tsc": b"fixture tsc",
            "share/nudox/typescript/node_modules/typescript/lib/typescript.js": b"fixture TypeScript API",
            "share/nudox/typescript/node_modules/typescript/LICENSE.txt": b"TypeScript license",
            "share/nudox/typescript/node_modules/typescript/ThirdPartyNoticeText.txt": b"notices",
        }
        records = {}
        for relative, payload in contents.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(payload)
            records[relative] = {"kind": "file", "sha256": hashlib.sha256(payload).hexdigest(),
                                 "size_bytes": len(payload)}
        node_relative = "share/nudox/typescript/node/bin/node"
        os.chmod(root / node_relative, 0o755)
        return {"schema": "nudox.typescript-sdk.v1", "bundled": True,
                "root": "share/nudox/typescript", "files": records,
                "node": {"packaged_path": node_relative,
                         "packaged_sha256": records[node_relative]["sha256"]},
                "node_version": "v24.18.0", "typescript_version": "5.9.3"}

    def test_sdk_probe_preserves_nixos_stub_failure_instead_of_claiming_version_mismatch(self):
        diagnostic = ("Could not start dynamically linked executable: /prefix/node\n"
                      "NixOS cannot run dynamically linked executables intended for generic\n"
                      "linux environments out of the box. For more information, see:\n"
                      "https://nix.dev/permalink/stub-ld\n")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            sdk = self._sdk_fixture(root)
            result = type("ProbeResult", (), {"returncode": 127, "stdout": "", "stderr": diagnostic})()
            with patch.object(installer.subprocess, "run", return_value=result) as run:
                with self.assertRaises(installer.InstallError) as raised:
                    installer.verify_typescript_sdk(root, {"typescript_sdk": sdk})
            run.assert_called_once()
            message = str(raised.exception)
            self.assertIn("Node --version", message)
            self.assertIn("returncode=127", message)
            self.assertIn("stdout=<empty>", message)
            self.assertIn(repr(diagnostic), message)
            self.assertIn("loader reports that generic Linux dynamically linked executables are unsupported", message)
            self.assertNotIn("reported version=", message)

    def test_sdk_probe_reports_an_actual_recorded_version_mismatch(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            sdk = self._sdk_fixture(root)
            result = type("ProbeResult", (), {"returncode": 0, "stdout": "v24.18.1\n", "stderr": ""})()
            with patch.object(installer.subprocess, "run", return_value=result):
                with self.assertRaises(installer.InstallError) as raised:
                    installer.verify_typescript_sdk(root, {"typescript_sdk": sdk})
            message = str(raised.exception)
            self.assertIn("expected='v24.18.0'", message)
            self.assertIn("reported version='v24.18.1'", message)
            self.assertIn("returncode=0", message)
            self.assertIn("stderr=<empty>", message)
            self.assertNotIn("NixOS", message)

    def test_sdk_probe_timeout_keeps_partial_output_in_diagnostic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            sdk = self._sdk_fixture(root)
            timeout = installer.subprocess.TimeoutExpired(
                ["node", "--version"], 15, output=b"partial stdout", stderr=b"partial stderr")
            with patch.object(installer.subprocess, "run", side_effect=timeout):
                with self.assertRaises(installer.InstallError) as raised:
                    installer.verify_typescript_sdk(root, {"typescript_sdk": sdk})
            message = str(raised.exception)
            self.assertIn("timed out after 15 seconds", message)
            self.assertIn("returncode=unavailable", message)
            self.assertIn("stdout='partial stdout'", message)
            self.assertIn("stderr='partial stderr'", message)

    def test_sdk_probe_failure_clips_rendered_diagnostics_and_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            sdk = self._sdk_fixture(root)
            sdk["node_version"] = "expected-version-" + "e" * 5000
            result = type("ProbeResult", (), {"returncode": 127, "stdout": "o" * 5000,
                                               "stderr": "s" * 5000})()
            with patch.object(installer.subprocess, "run", return_value=result):
                with self.assertRaises(installer.InstallError) as raised:
                    installer.verify_typescript_sdk(root, {"typescript_sdk": sdk})
            message = str(raised.exception)
            self.assertLess(len(message), 3 * installer._SDK_PROBE_OUTPUT_LIMIT + 1024)
            self.assertIn("returncode=127", message)
            self.assertEqual(message.count("<truncated>"), 3)

    def test_sdk_probe_refusal_leaves_existing_install_pointer_and_marker_unchanged(self):
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory).resolve()
            managed_root = prefix / "lib/nudox"
            old_release = managed_root / "versions/checkpoint-20261008-aaaaaaaaaa-linux-x64"
            old_release.mkdir(parents=True, mode=0o700)
            marker = {"version": "0.2.0", "tag": "checkpoint-20261008-aaaaaaaaaa-linux-x64",
                      "source_sha": "a" * 40, "asset": "nudox-linux-x86_64-aaaaaaaaaa.tar.gz",
                      "sha256": "c" * 64}
            marker_path = old_release / ".installed-release.json"
            marker_path.write_text(json.dumps(marker, sort_keys=True) + "\n")
            current = managed_root / "current"
            current.symlink_to("versions/" + old_release.name)
            old_marker_bytes = marker_path.read_bytes()
            old_target = os.readlink(current)
            incoming = {"version": "0.2.1", "tag": "checkpoint-20261009-bbbbbbbbbb-linux-x64",
                        "source_sha": "b" * 40, "asset": "nudox-linux-x86_64-bbbbbbbbbb.tar.gz"}
            manifest = {"sha256": "d" * 64}
            archive = prefix / "unused.tar.gz"
            archive.write_bytes(b"fixture archive placeholder")
            diagnostic = ("Could not start dynamically linked executable: /stage/node\n"
                          "NixOS cannot run dynamically linked executables intended for generic\n"
                          "linux environments out of the box. For more information, see:\n"
                          "https://nix.dev/permalink/stub-ld\n")
            result = type("ProbeResult", (), {"returncode": 127, "stdout": "", "stderr": diagnostic})()
            saved_umask = os.umask(0o077)
            try:
                with patch.object(installer, "safe_extract") as extract, \
                     patch.object(installer, "verify_package", side_effect=lambda stage, _entry, _manifest:
                                   installer.verify_typescript_sdk(stage, {"typescript_sdk": self._sdk_fixture(stage)})), \
                     patch.object(installer.subprocess, "run", return_value=result):
                    with self.assertRaisesRegex(installer.InstallError, "returncode=127"):
                        installer.install(prefix, incoming, manifest, archive)
            finally:
                os.umask(saved_umask)
            extract.assert_called_once()
            self.assertTrue(current.is_symlink())
            self.assertEqual(os.readlink(current), old_target)
            self.assertEqual(marker_path.read_bytes(), old_marker_bytes)
            self.assertFalse((managed_root / "versions" / incoming["tag"]).exists())
            self.assertEqual(list((managed_root / "versions").glob(".staging-*")), [])
            self.assertFalse((prefix / "bin/nudox").exists())

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

    def test_hash_refuses_fifo_swap_after_path_admission_and_oversized_sparse_file(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            member = root / "member"
            member.write_bytes(b"admitted regular file")
            installer._verified_package_path(root, "member")
            real_open = os.open

            def swap_before_open(path, flags, *arguments, **keywords):
                if Path(path) == member:
                    member.rename(root / "original")
                    os.mkfifo(member)
                return real_open(path, flags, *arguments, **keywords)

            with patch.object(installer.os, "open", side_effect=swap_before_open):
                with self.assertRaisesRegex(installer.InstallError, "bounded regular file"):
                    installer._file_sha256(member)
            member.unlink()
            with member.open("wb") as stream:
                stream.truncate(8 * 1024 * 1024 * 1024)
            with patch.object(installer.hashlib, "sha256") as hashing:
                with self.assertRaisesRegex(installer.InstallError, "bounded regular file"):
                    installer._file_sha256(member)
                hashing.assert_not_called()

    def test_installed_sdk_refuses_runtime_package_bound_before_hashing_oversize_member(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            relative = "share/nudox/typescript/node_modules/typescript/lib/typescript.js"
            api = root / relative
            api.parent.mkdir(parents=True)
            with api.open("wb") as file:
                file.truncate(96 * 1024 * 1024 + 1)
            sdk = {"schema": "nudox.typescript-sdk.v1", "bundled": True,
                   "root": "share/nudox/typescript",
                   "files": {relative: {"kind": "file", "sha256": "a" * 64,
                                         "size_bytes": api.stat().st_size}}}
            with patch.object(installer, "_file_sha256") as hash_payload:
                with self.assertRaisesRegex(installer.InstallError, "TypeScript package exceeds its 96 MiB"):
                    installer.verify_typescript_sdk(root, {"typescript_sdk": sdk})
                hash_payload.assert_not_called()

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
