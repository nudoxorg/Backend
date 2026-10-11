"""Synthetic candidate-composition controls; none of these fixtures are releases."""
import copy
import hashlib
import json
from dataclasses import replace
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import compose_forgejo_checkpoint as compose


ROOT = Path(__file__).parent


def synthetic_inputs():
    base_raw = (ROOT / "forgejo-preview-channel.json").read_bytes()
    base = json.loads(base_raw)
    template = (ROOT / "install_forgejo_preview.py").read_bytes()
    mac_library = (ROOT / "install_macos_preview.py").read_bytes()
    source = compose.SourceSnapshot(ROOT, "f" * 40, "e" * 40, template, mac_library,
                                    base, hashlib.sha256(base_raw).hexdigest())
    manifest = {
        "schema": 1,
        "version": "0.2.0",
        "source_sha": "a" * 40,
        "source_tree": "b" * 40,
        "cargo_lock_sha256": "c" * 64,
        "platform": "linux-x64",
        "target": "x86_64-unknown-linux-gnu",
        "asset": "nudox-linux-x86_64-aaaaaaaaaa.tar.gz",
        "sha256": "d" * 64,
        "size_bytes": 17,
        "minimum_glibc": "2.39",
        "build_manifest_sha256": "1" * 64,
        "packaging_manifest_sha256": "2" * 64,
    }
    manifest_bytes = json.dumps(manifest, separators=(",", ":")).encode()
    manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
    tag = "checkpoint-20261011-aaaaaaaaaa-linux-x64"
    installer_template = (ROOT / "install_linux.py").read_bytes()
    installer = compose.pin_linux_installer(installer_template, tag, manifest_sha)
    package = compose.LinuxPackage(
        directory=ROOT,
        manifest=manifest,
        manifest_bytes=manifest_bytes,
        manifest_sha256=manifest_sha,
        tag=tag,
        archive=ROOT / manifest["asset"],
        archive_sha256=manifest["sha256"],
        archive_bytes=manifest["size_bytes"],
        installer=installer,
        installer_source_template=installer_template,
        native_qa_template_sha256="3" * 64,
        native_qa_limitation="TEST ONLY: native package acceptance is not claimed.",
    )
    return source, package


class CandidateCompositionTests(unittest.TestCase):
    def test_candidate_pins_and_channel_are_composed_from_typed_inputs(self):
        source, package = synthetic_inputs()
        original_mac = copy.deepcopy(source.base_channel["platforms"]["macos-arm64"])
        result = compose.compose_candidate(source, package)
        pins = {key: value.bootstrap_pin for key, value in result.libraries.items()}
        constants = compose.literal_assignments(
            result.bootstrap, {"BOOTSTRAP_TAG", "CHANNEL", "LIBRARIES"})
        self.assertEqual(constants["BOOTSTRAP_TAG"], result.bootstrap_tag)
        self.assertEqual(constants["CHANNEL"], compose.bootstrap.release_url(
            result.bootstrap_tag, "preview-channel.json"))
        self.assertEqual(constants["LIBRARIES"], pins)
        self.assertEqual(result.channel["platforms"]["linux-x64"]["source"], package.manifest["source_sha"])
        self.assertEqual(result.channel["platforms"]["linux-x64"]["tag"], package.tag)
        self.assertEqual(result.channel["platforms"]["linux-x64"]["manifest"]["sha256"], package.manifest_sha256)
        self.assertEqual(result.channel["platforms"]["linux-x64"]["archive"]["sha256"], package.archive_sha256)
        mac = result.channel["platforms"]["macos-arm64"]
        for field in ("source", "tag", "archive", "known_failures"):
            self.assertEqual(mac[field], original_mac[field])
        self.assertEqual(source.base_channel["platforms"]["macos-arm64"], original_mac)
        compose.bootstrap.validate_channel(result.channel, bootstrap_tag=result.bootstrap_tag,
            libraries=pins)
        compose.bootstrap.admit_libraries(result.channel, libraries=pins)
        wrong_library_location = copy.deepcopy(result.channel)
        installer = wrong_library_location["platforms"]["macos-arm64"]["installer"]
        installer["url"] = compose.bootstrap.release_url("foreign-release", Path(installer["url"]).name)
        with self.assertRaises(compose.bootstrap.InstallError):
            compose.bootstrap.validate_channel(wrong_library_location, bootstrap_tag=result.bootstrap_tag,
                                                libraries=pins)

    def test_manifest_source_tag_and_linux_library_must_share_one_identity(self):
        source, package = synthetic_inputs()
        with self.assertRaisesRegex(compose.CandidateError, "manifest bytes"):
            compose.compose_candidate(source, replace(package, manifest_sha256="4" * 64))
        with self.assertRaisesRegex(compose.CandidateError, "identity changed"):
            compose.compose_candidate(source, replace(package, tag="checkpoint-20261011-bbbbbbbbbb-linux-x64"))
        corrupt_library = package.installer + b"# changed after source admission\n"
        with self.assertRaisesRegex(compose.CandidateError, "selected source and manifest"):
            compose.compose_candidate(source, replace(package, installer=corrupt_library))
        changed_source = dict(package.manifest, source_sha="b" * 40)
        with self.assertRaisesRegex(compose.CandidateError, "checkpoint tag differs"):
            compose.compose_candidate(source, replace(package, manifest=changed_source))

    def test_mac_library_and_supported_platform_set_are_closed(self):
        source, package = synthetic_inputs()
        self.assertIn("reuse_existing_install", compose.function_names(source.mac_library))
        bad_mac = replace(source, mac_library=b"PLATFORM = 'linux-x64'\n")
        with self.assertRaisesRegex(compose.CandidateError, "different platform"):
            compose.compose_candidate(bad_mac, package)
        extra_platforms = copy.deepcopy(source.base_channel)
        extra_platforms["platforms"]["linux-arm64"] = {}
        bad_platforms = replace(source, base_channel=extra_platforms)
        with self.assertRaisesRegex(compose.CandidateError, "channel and bootstrap pins disagree"):
            compose.compose_candidate(bad_platforms, package)

    def test_linux_package_root_symlink_is_refused_before_resolution(self):
        source, _ = synthetic_inputs()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            real_package = root / "real-package"
            real_package.mkdir()
            alias = root / "package-alias"
            alias.symlink_to(real_package, target_is_directory=True)
            with self.assertRaisesRegex(compose.CandidateError, "must not be a symlink"):
                compose.validate_linux_package(alias, source)

    def test_copy_refuses_preexisting_destination_without_removing_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.tar.gz"
            destination = root / "destination.tar.gz"
            source.write_bytes(b"expected candidate bytes")
            destination.write_bytes(b"preserve existing destination")
            before = destination.stat()

            with self.assertRaises(FileExistsError):
                compose.copy_verified(source, destination, hashlib.sha256(source.read_bytes()).hexdigest(),
                                      source.stat().st_size)

            after = destination.stat()
            self.assertEqual((after.st_dev, after.st_ino), (before.st_dev, before.st_ino))
            self.assertEqual(destination.read_bytes(), b"preserve existing destination")

    def test_copy_failure_preserves_replacement_inode(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.tar.gz"
            destination = root / "destination.tar.gz"
            source.write_bytes(b"expected candidate bytes")
            replacement = b"a different process replacement"
            real_stat = compose.os.stat
            swapped = False

            def replace_before_name_check(path, *args, **kwargs):
                nonlocal swapped
                if path == destination.name and kwargs.get("dir_fd") is not None and not swapped:
                    destination.unlink()
                    destination.write_bytes(replacement)
                    swapped = True
                return real_stat(path, *args, **kwargs)

            with mock.patch.object(compose.os, "stat", side_effect=replace_before_name_check):
                with self.assertRaisesRegex(compose.CandidateError, "staged candidate package asset"):
                    compose.copy_verified(source, destination, hashlib.sha256(source.read_bytes()).hexdigest(),
                                          source.stat().st_size)

            self.assertTrue(swapped)
            self.assertEqual(destination.read_bytes(), replacement)

    def test_copy_failure_retains_partial_asset_for_inspection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source.tar.gz"
            destination = root / "destination.tar.gz"
            expected = b"expected candidate bytes"
            source.write_bytes(expected)

            with self.assertRaisesRegex(compose.CandidateError, "changed while copying"):
                compose.copy_verified(source, destination, "0" * 64, len(expected))

            self.assertEqual(destination.read_bytes(), expected)

    def test_failed_output_is_retained_for_inspection(self):
        source, package = synthetic_inputs()
        bundle = compose.compose_candidate(source, package)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "candidate"
            with mock.patch.object(compose, "write_exclusive", side_effect=OSError("injected first-write failure")):
                with self.assertRaisesRegex(compose.CandidateError, "private output was retained"):
                    compose.prepare_output(bundle, output)

            self.assertTrue(output.is_dir())
            self.assertEqual(list(output.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
