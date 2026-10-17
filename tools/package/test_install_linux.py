"""Data and filesystem regressions for the Linux installer; no product ELF is faked."""
import hashlib
import io
import json
import os
from pathlib import Path, PurePath
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
    def _fixture_release(root, source_sha, date, *, include_desktop=False):
        archive = Path(root) / f"{date}-{source_sha[:10]}.tar.gz"
        files = {
            "bin/backend-cli": b"fixture cli image",
            "bin/backend-mcp": b"fixture mcp image",
            "bin/backend-locald": b"fixture locald image",
            "README.txt": b"fixture package readme",
        }
        if include_desktop:
            files["bin/backend-desktop"] = b"fixture desktop image"
        build_bytes = json.dumps({"source_sha": source_sha}, sort_keys=True).encode()
        package_value = {
            "schema": "nudox.linux-portable-package.v1",
            "executables": [
                {"name": name[4:] if name.startswith("bin/") else name, "packaged_path": name,
                 "packaged_sha256": hashlib.sha256(body).hexdigest()}
                for name, body in files.items() if name.startswith("bin/")
            ],
            "libraries": {},
        }
        if include_desktop:
            package_value.update(schema="nudox.linux-portable-package.v2", desktop=dict(installer.DESKTOP_ROLE))
        package_bytes = json.dumps(package_value, sort_keys=True).encode()
        files["build-manifest.json"] = build_bytes
        files["packaging-manifest.json"] = package_bytes
        with tarfile.open(archive, "w:gz") as bundle:
            root_member = tarfile.TarInfo("nudox-linux-x86_64/")
            root_member.type = tarfile.DIRTYPE
            bundle.addfile(root_member)
            for relative, body in files.items():
                member = tarfile.TarInfo("nudox-linux-x86_64/" + relative)
                member.size = len(body)
                member.mode = 0o755 if relative.startswith("bin/") else 0o644
                bundle.addfile(member, io.BytesIO(body))
        entry = {
            "version": "0.2.0",
            "tag": f"checkpoint-{date}-{source_sha[:10]}-linux-x64",
            "source_sha": source_sha,
            "asset": f"nudox-linux-x86_64-{source_sha[:10]}.tar.gz",
        }
        manifest = {
            "sha256": hashlib.sha256(archive.read_bytes()).hexdigest(),
            "size_bytes": archive.stat().st_size,
            "minimum_glibc": "2.17",
            "build_manifest_sha256": hashlib.sha256(build_bytes).hexdigest(),
            "packaging_manifest_sha256": hashlib.sha256(package_bytes).hexdigest(),
        }
        if include_desktop:
            manifest.update(schema=2, desktop=dict(installer.DESKTOP_ROLE))
        return entry, manifest, archive

    def _install_fixture_release(self, prefix, entry, manifest, archive):
        saved_umask = os.umask(0o077)
        try:
            with patch.object(installer, "_glibc_version", return_value=(99, 0)), \
                 patch.object(installer.subprocess, "run", return_value=type("Result", (), {"returncode": 0, "stdout": "ok", "stderr": ""})()):
                installer.install(prefix, entry, manifest, archive)
        finally:
            os.umask(saved_umask)

    def test_install_restores_callers_umask_after_success_and_error(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            entry, manifest, archive = self._fixture_release(root, "d" * 40, "20261010")
            prefix = root / "prefix"
            for bad_entry in (False, True):
                with self.subTest(bad_entry=bad_entry):
                    saved_umask = os.umask(0o027)
                    try:
                        selected = {**entry, "tag": "../invalid"} if bad_entry else entry
                        with patch.object(installer, "_glibc_version", return_value=(99, 0)), \
                             patch.object(installer.subprocess, "run", return_value=type("Result", (), {"returncode": 0, "stdout": "ok", "stderr": ""})()):
                            if bad_entry:
                                with self.assertRaises(installer.InstallError):
                                    installer.install(prefix, selected, manifest, archive)
                            else:
                                installer.install(prefix, selected, manifest, archive)
                        observed_umask = os.umask(0o027)
                        self.assertEqual(observed_umask, 0o027)
                    finally:
                        os.umask(saved_umask)

    def _run_channel_main(self, prefix, entry, manifest, *, download_archive):
        from types import SimpleNamespace
        with patch.object(installer.sys, "platform", "linux"), \
             patch.object(installer.os, "uname", return_value=SimpleNamespace(machine="x86_64")), \
             patch.object(installer, "load_release", return_value=(entry, manifest, "https://example.invalid/archive")), \
             patch.object(installer, "download_archive", side_effect=download_archive) as download, \
             patch.object(installer, "_glibc_version", return_value=(99, 0)), \
             patch.object(installer.subprocess, "run", return_value=type("Result", (), {"returncode": 0, "stdout": "ok", "stderr": ""})()):
            result = installer.main(["--prefix", str(prefix)])
        return result, download

    def test_known_nixos_stub_loader_is_refused_before_channel_or_archive_download(self):
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as directory:
            prefix = Path(directory).resolve() / "prefix"
            with patch.object(installer.sys, "platform", "linux"), \
                 patch.object(installer.os, "uname", return_value=SimpleNamespace(machine="x86_64")), \
                 patch.object(installer, "_system_elf_loader_target", return_value="/nix/store/xphm1v1snir05ajsk4v1zslzxmmsfy6g-stub-ld-x86_64-unknown-linux-musl"), \
                 patch.object(installer, "load_release") as load_release, \
                 patch.object(installer, "download_archive") as download_archive, \
                 patch.dict(os.environ, {"HOME": directory}):
                with self.assertRaisesRegex(installer.InstallError, "resolves to the NixOS generic stub loader.*no release archive was downloaded"):
                    installer.main(["--prefix", str(prefix)])
            load_release.assert_not_called()
            download_archive.assert_not_called()
            self.assertFalse(prefix.exists())

    def test_real_glibc_and_nix_ld_interpreters_are_not_refused(self):
        from types import SimpleNamespace
        for target in ("/lib64/ld-linux-x86-64.so.2", "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-nix-ld", None):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as directory:
                with patch.object(installer.sys, "platform", "linux"), \
                     patch.object(installer.os, "uname", return_value=SimpleNamespace(machine="x86_64")), \
                     patch.object(installer, "_system_elf_loader_target", return_value=target), \
                     patch.object(installer, "load_release", side_effect=RuntimeError("reached release lookup")) as load_release, \
                     patch.dict(os.environ, {"HOME": directory}):
                    with self.assertRaisesRegex(RuntimeError, "reached release lookup"):
                        installer.main(["--prefix", str(Path(directory) / "prefix")])
                load_release.assert_called_once()

    def test_system_elf_loader_target_resolves_symlink_without_running_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            target = root / installer.KNOWN_NIXOS_STUB_INTERPRETER
            target.write_bytes(b"not executed")
            link = root / "ld-linux-x86-64.so.2"
            link.symlink_to(target)
            with patch.object(installer, "SYSTEM_ELF_INTERPRETER", link):
                self.assertEqual(installer._system_elf_loader_target(), target.resolve().as_posix())

    def test_known_stub_match_requires_nix_store_hash_for_resolved_paths(self):
        self.assertFalse(installer._is_known_nixos_stub_target(installer.KNOWN_NIXOS_STUB_INTERPRETER))
        self.assertTrue(installer._is_known_nixos_stub_target(
            "/nix/store/xphm1v1snir05ajsk4v1zslzxmmsfy6g-stub-ld-x86_64-unknown-linux-musl"))
        self.assertFalse(installer._is_known_nixos_stub_target(
            "/tmp/xphm1v1snir05ajsk4v1zslzxmmsfy6g-stub-ld-x86_64-unknown-linux-musl"))
        self.assertFalse(installer._is_known_nixos_stub_target(
            "/nix/store/invalidhash-stub-ld-x86_64-unknown-linux-musl"))

    def test_channel_rerun_skips_archive_only_after_receipt_and_file_recheck(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            prefix = root / "prefix"
            entry, manifest, archive = self._fixture_release(root, "a" * 40, "20261013")
            self._install_fixture_release(prefix, entry, manifest, archive)
            current = prefix / "lib/nudox/current"
            before_target = os.readlink(current)
            marker_path = current.resolve() / ".installed-release.json"
            before_marker = marker_path.read_bytes()

            result, download = self._run_channel_main(
                prefix, entry, manifest,
                download_archive=AssertionError("verified same-release rerun downloaded its archive"),
            )

            self.assertEqual(result, 0)
            download.assert_not_called()
            self.assertEqual(os.readlink(current), before_target)
            self.assertEqual(marker_path.read_bytes(), before_marker)
            for name in installer.command_links(prefix / "lib/nudox"):
                self.assertTrue((prefix / "bin" / name).is_symlink())

    def test_tampered_required_files_and_metadata_refuse_reuse_before_download(self):
        for relative in ("bin/backend-cli", "bin/backend-mcp", "bin/backend-locald",
                         "build-manifest.json", "packaging-manifest.json"):
            with self.subTest(relative=relative), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                prefix = root / "prefix"
                entry, manifest, archive = self._fixture_release(root, "a" * 40, "20261013")
                self._install_fixture_release(prefix, entry, manifest, archive)
                current = prefix / "lib/nudox/current"
                before_target = os.readlink(current)
                marker_path = current.resolve() / ".installed-release.json"
                before_marker = marker_path.read_bytes()
                payload = current.resolve().joinpath(*relative.split("/"))
                payload.write_bytes(payload.read_bytes() + b" tampered")

                with self.assertRaisesRegex(installer.InstallError, "integrity/startup recheck"):
                    self._run_channel_main(
                        prefix, entry, manifest,
                        download_archive=AssertionError("tampered installed package was redownloaded"),
                    )

                self.assertEqual(os.readlink(current), before_target)
                self.assertEqual(marker_path.read_bytes(), before_marker)

    def test_update_and_archive_failure_keep_the_existing_release_atomic(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            prefix = root / "prefix"
            old_entry, old_manifest, old_archive = self._fixture_release(root, "a" * 40, "20261010")
            self._install_fixture_release(prefix, old_entry, old_manifest, old_archive)
            current = prefix / "lib/nudox/current"
            before_target = os.readlink(current)
            before_marker = (current.resolve() / ".installed-release.json").read_bytes()
            new_entry, new_manifest, new_archive = self._fixture_release(root, "b" * 40, "20261013")

            with self.assertRaisesRegex(installer.InstallError, "archive SHA-256 mismatch"):
                self._run_channel_main(
                    prefix, new_entry, new_manifest,
                    download_archive=installer.InstallError("archive SHA-256 mismatch"),
                )
            self.assertEqual(os.readlink(current), before_target)
            self.assertEqual((current.resolve() / ".installed-release.json").read_bytes(), before_marker)
            self.assertFalse((prefix / "lib/nudox/versions" / new_entry["tag"]).exists())
            versions = prefix / "lib/nudox/versions"
            self.assertEqual(list(versions.glob(".staging-*")), [])

            def deliver_corrupt_archive(_url, _digest, _size, destination):
                Path(destination).write_bytes(b"not a gzip archive")

            with self.assertRaisesRegex(installer.InstallError, "could not safely extract release archive"):
                self._run_channel_main(
                    prefix, new_entry, new_manifest,
                    download_archive=deliver_corrupt_archive,
                )
            self.assertEqual(os.readlink(current), before_target)
            self.assertEqual((current.resolve() / ".installed-release.json").read_bytes(), before_marker)
            self.assertFalse((versions / new_entry["tag"]).exists())
            self.assertEqual(list(versions.glob(".staging-*")), [])

            def deliver_fixture(_url, _digest, _size, destination):
                Path(destination).write_bytes(new_archive.read_bytes())

            result, download = self._run_channel_main(
                prefix, new_entry, new_manifest, download_archive=deliver_fixture,
            )
            self.assertEqual(result, 0)
            download.assert_called_once()
            self.assertNotEqual(os.readlink(current), before_target)
            self.assertEqual(os.readlink(current), "versions/" + new_entry["tag"])
            self.assertTrue((prefix / "lib/nudox/versions" / old_entry["tag"]).is_dir())

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

    def test_package_path_containment_does_not_require_python39_is_relative_to(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary).resolve()
            root = base / "package"
            (root / "bin").mkdir(parents=True)
            executable = root / "bin/backend-cli"
            executable.write_bytes(b"fixture")

            # Simulate the Python 3.8 pathlib API, where PurePath.is_relative_to is absent.
            # If the implementation regresses to calling it, this test raises TypeError.
            with patch.object(PurePath, "is_relative_to", None, create=True):
                self.assertEqual(installer._verified_package_path(root, "bin/backend-cli"), executable)
                with self.assertRaisesRegex(installer.InstallError, "unsafe component"):
                    installer._verified_package_path(root, "../outside")

            alias = base / "package-alias"
            alias.symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(installer.InstallError, "not a canonical real directory"):
                installer._verified_package_path(alias, "bin/backend-cli")

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


class DesktopInstallerTests(unittest.TestCase):
    fixture = staticmethod(InstallerFilesystemTests._fixture_release)

    def install_fixture(self, prefix, entry, manifest, archive):
        with patch.object(installer, "_glibc_version", return_value=(99, 0)), \
             patch.object(installer.subprocess, "run", return_value=type("Result", (), {"returncode": 0, "stdout": "ok", "stderr": ""})()) as probes:
            installer.install(prefix, entry, manifest, archive)
        return probes

    def test_schema_two_requires_exact_explicit_role_and_legacy_cannot_opt_in(self):
        for manifest in ({"schema": 2}, {"schema": 2, "desktop": {"executable": "backend-cli", "launcher": "nudox-desktop"}},
                         {"schema": 1, "desktop": dict(installer.DESKTOP_ROLE)}):
            with self.subTest(manifest=manifest), self.assertRaises(installer.InstallError):
                installer.validate_release_manifest(manifest)
        self.assertEqual(installer.release_binaries({"schema": 2, "desktop": dict(installer.DESKTOP_ROLE)}),
                         (*installer.REQUIRED_BINARIES, "backend-desktop"))
        self.assertEqual(installer.release_binaries({"schema": 1}), installer.REQUIRED_BINARIES)

    def test_desktop_install_hashes_fourth_executable_and_exposes_explicit_aliases_without_gui_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            entry, manifest, archive = self.fixture(root, "a" * 40, "20261017", include_desktop=True)
            prefix = root / "prefix"
            probes = self.install_fixture(prefix, entry, manifest, archive)
            current = prefix / "lib/nudox/current"
            self.assertTrue(os.access(current / "bin/backend-desktop", os.X_OK))
            for name in ("backend-desktop", "nudox-desktop"):
                self.assertEqual((prefix / "bin" / name).resolve(), (current / "bin/backend-desktop").resolve())
            self.assertEqual((prefix / "bin/nudox").resolve(), (current / "bin/backend-cli").resolve())
            self.assertEqual([Path(call.args[0][0]).name for call in probes.call_args_list], ["backend-cli", "backend-mcp"])
            (current / "bin/backend-desktop").write_bytes(b"tampered desktop")
            with self.assertRaisesRegex(installer.InstallError, "integrity/startup recheck"):
                installer.reuse_existing_install(prefix, entry, manifest)

    def test_desktop_release_missing_fourth_executable_refuses_before_extracting(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            _, manifest, archive = self.fixture(root, "a" * 40, "20261017")
            manifest.update(schema=2, desktop=dict(installer.DESKTOP_ROLE))
            stage = root / "stage"
            stage.mkdir()
            with self.assertRaisesRegex(installer.InstallError, "missing one or more required"):
                installer.safe_extract(archive, stage, manifest=manifest)
            self.assertEqual(list(stage.iterdir()), [])

    def test_legacy_release_cannot_silently_extract_desktop(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            _, _, archive = self.fixture(root, "a" * 40, "20261017", include_desktop=True)
            stage = root / "stage"
            stage.mkdir()
            with self.assertRaisesRegex(installer.InstallError, "explicit schema-2"):
                installer.safe_extract(archive, stage)
            self.assertEqual(list(stage.iterdir()), [])

    def test_package_desktop_role_mismatch_and_duplicate_inventory_refuse_before_probes(self):
        for mutation in ("role", "duplicate", "glibc-floor"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as directory:
                root = Path(directory).resolve()
                entry, manifest, archive = self.fixture(root, "a" * 40, "20261017", include_desktop=True)
                stage = root / "stage"
                stage.mkdir()
                installer.safe_extract(archive, stage, manifest=manifest)
                path = stage / "packaging-manifest.json"
                package_value = json.loads(path.read_text())
                if mutation == "role":
                    package_value["desktop"]["launcher"] = "nudox"
                elif mutation == "duplicate":
                    package_value["executables"].append(package_value["executables"][-1])
                else:
                    manifest["minimum_glibc"] = "2.42"
                path.write_text(json.dumps(package_value))
                manifest["packaging_manifest_sha256"] = hashlib.sha256(path.read_bytes()).hexdigest()
                with patch.object(installer.subprocess, "run") as probes, \
                     patch.object(installer, "_glibc_version", return_value=(2, 39)):
                    with self.assertRaises(installer.InstallError):
                        installer.verify_package(stage, entry, manifest)
                probes.assert_not_called()

    def test_switch_to_cli_removes_only_owned_desktop_aliases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            prefix = root / "prefix"
            self.install_fixture(prefix, *self.fixture(root, "a" * 40, "20261016", include_desktop=True))
            self.install_fixture(prefix, *self.fixture(root, "b" * 40, "20261017"))
            for name in ("nudox-desktop", "backend-desktop"):
                self.assertFalse((prefix / "bin" / name).is_symlink())
            self.assertTrue((prefix / "bin/nudox").resolve().is_file())

    def test_failed_current_activation_preserves_previous_desktop_aliases(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            prefix = root / "prefix"
            self.install_fixture(prefix, *self.fixture(root, "a" * 40, "20261016", include_desktop=True))
            current = prefix / "lib/nudox/current"
            before = os.readlink(current)
            aliases = {prefix / "bin" / name: os.readlink(prefix / "bin" / name)
                       for name in ("nudox-desktop", "backend-desktop")}
            previous_desktop = (current / "bin/backend-desktop").resolve()
            real_replace = installer._replace_symlink
            def fail_current(path, target):
                if path == current:
                    raise OSError("injected atomic current activation failure")
                return real_replace(path, target)
            with patch.object(installer, "_replace_symlink", side_effect=fail_current):
                with self.assertRaisesRegex(OSError, "injected atomic current activation failure"):
                    self.install_fixture(prefix, *self.fixture(root, "b" * 40, "20261017"))
            self.assertEqual(os.readlink(current), before)
            for alias, target in aliases.items():
                self.assertTrue(alias.is_symlink())
                self.assertEqual(os.readlink(alias), target)
                self.assertEqual(alias.resolve(), previous_desktop)
            self.assertEqual(previous_desktop.read_bytes(), b"fixture desktop image")

    def test_desktop_alias_conflict_preserves_current_cli_install(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            prefix = root / "prefix"
            self.install_fixture(prefix, *self.fixture(root, "a" * 40, "20261016"))
            current = prefix / "lib/nudox/current"
            before = os.readlink(current)
            (prefix / "bin/nudox-desktop").write_bytes(b"user-owned launcher")
            with self.assertRaisesRegex(installer.InstallError, "non-symlink command"):
                self.install_fixture(prefix, *self.fixture(root, "b" * 40, "20261017", include_desktop=True))
            self.assertEqual(os.readlink(current), before)
            self.assertEqual((prefix / "bin/nudox-desktop").read_bytes(), b"user-owned launcher")


if __name__ == "__main__":
    unittest.main()
