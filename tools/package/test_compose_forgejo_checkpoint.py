"""Synthetic candidate-composition controls; none of these fixtures are releases."""
import copy
import hashlib
import json
from dataclasses import replace
import os
from pathlib import Path
import subprocess
import sys
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
    launcher_template = (ROOT / "install_diagnostic_preview.sh").read_bytes()
    source = compose.SourceSnapshot(ROOT, "f" * 40, "e" * 40, template, mac_library,
                                    launcher_template, base, hashlib.sha256(base_raw).hexdigest())
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
        failures = result.channel["platforms"]["linux-x64"]["known_failures"]
        self.assertEqual(failures, [package.native_qa_limitation])
        self.assertNotIn("October 8", " ".join(failures))
        for prior_failure in source.base_channel["platforms"]["linux-x64"]["known_failures"]:
            self.assertNotIn(prior_failure, failures)
        self.assertFalse(result.channel["production_ready"])
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

    def test_same_linux_source_and_archive_preserve_detailed_base_failures(self):
        source, package = synthetic_inputs()
        base_row = source.base_channel["platforms"]["linux-x64"]
        manifest = dict(
            package.manifest,
            source_sha=base_row["source"],
            asset=Path(base_row["archive"]["url"]).name,
            sha256=base_row["archive"]["sha256"],
            size_bytes=base_row["archive"]["bytes"],
        )
        manifest_bytes = json.dumps(manifest, separators=(",", ":")).encode()
        manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
        tag = "checkpoint-20261011-" + manifest["source_sha"][:10] + "-linux-x64"
        installer = compose.pin_linux_installer(package.installer_source_template, tag, manifest_sha)
        same_artifact = replace(
            package,
            manifest=manifest,
            manifest_bytes=manifest_bytes,
            manifest_sha256=manifest_sha,
            tag=tag,
            archive_sha256=manifest["sha256"],
            archive_bytes=manifest["size_bytes"],
            installer=installer,
        )

        result = compose.compose_candidate(source, same_artifact)
        failures = result.channel["platforms"]["linux-x64"]["known_failures"]
        self.assertEqual(failures[:-1], base_row["known_failures"])
        self.assertEqual(failures[-1], same_artifact.native_qa_limitation)
        self.assertEqual(base_row["known_failures"], source.base_channel["platforms"]["linux-x64"]["known_failures"])

    def test_changed_source_or_archive_does_not_carry_old_linux_failures(self):
        source, package = synthetic_inputs()
        base_row = source.base_channel["platforms"]["linux-x64"]
        for changed in (
            replace(package, manifest=dict(package.manifest, source_sha=base_row["source"]),
                    archive_sha256=base_row["archive"]["sha256"]),
            replace(package, archive_sha256=base_row["archive"]["sha256"]),
            replace(package, archive_bytes=base_row["archive"]["bytes"]),
            replace(package, manifest=dict(package.manifest, source_sha=base_row["source"]),
                    archive_sha256=base_row["archive"]["sha256"],
                    archive_bytes=base_row["archive"]["bytes"] + 1),
        ):
            with self.subTest(source=changed.manifest["source_sha"], sha=changed.archive_sha256,
                              bytes=changed.archive_bytes):
                self.assertEqual(compose.linux_failures_for_candidate(base_row, changed), [])

    def test_uncompleted_native_qa_note_is_candidate_scoped_and_explicit(self):
        with tempfile.TemporaryDirectory() as directory:
            qa = Path(directory) / compose.NATIVE_QA_NAME
            qa.write_bytes((ROOT / "native-qa-linux-x64.example.json").read_bytes())
            _, limitation = compose.uncompleted_linux_qa(Path(directory))
        self.assertIn("No native Linux runtime acceptance is claimed for this release candidate", limitation)
        self.assertIn("these cases uncompleted", limitation)
        self.assertNotIn("October 8", limitation)

    def test_generated_launcher_is_digest_and_size_pinned_to_new_bootstrap(self):
        source, package = synthetic_inputs()
        result = compose.compose_candidate(source, package)
        body = result.launcher
        bootstrap_sha = hashlib.sha256(result.bootstrap).hexdigest()
        bootstrap_name = f"install-forgejo-bootstrap-{bootstrap_sha[:16]}.py"
        bootstrap_url = compose.bootstrap.release_url(result.bootstrap_tag, bootstrap_name)
        text = body.decode("utf-8")
        self.assertTrue(text.startswith("#!/bin/sh\n"))
        self.assertIn("installer_url='" + bootstrap_url + "'", text)
        self.assertIn("installer_sha256='" + bootstrap_sha + "'", text)
        self.assertIn("installer_bytes=" + str(len(result.bootstrap)) + "\n", text)
        self.assertIn("'" + result.bootstrap_tag + "' >&2", text)
        self.assertNotIn(compose.bootstrap.BOOTSTRAP_TAG, text)
        self.assertNotIn("historical R2", text)
        parsed = subprocess.run(["/bin/sh", "-n"], input=body, capture_output=True)
        self.assertEqual(parsed.returncode, 0, parsed.stderr.decode("utf-8", "replace"))

    def test_private_output_contains_digest_launcher_alias_and_false_readiness(self):
        source, package = synthetic_inputs()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            archive = root / package.archive.name
            archive_body = b"synthetic archive\n"
            archive.write_bytes(archive_body)
            archive_sha = hashlib.sha256(archive_body).hexdigest()
            package = replace(package, directory=root, archive=archive,
                              archive_sha256=archive_sha,
                              archive_bytes=archive.stat().st_size,
                              manifest=dict(package.manifest, sha256=archive_sha,
                                            size_bytes=archive.stat().st_size))
            # Keep all candidate identities internally consistent after the fixture size change.
            manifest_bytes = json.dumps(package.manifest, separators=(",", ":")).encode()
            manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
            tag = "checkpoint-20261011-" + package.manifest["source_sha"][:10] + "-linux-x64"
            installer = compose.pin_linux_installer(package.installer_source_template, tag, manifest_sha)
            checksum = root / (archive.name + ".sha256")
            checksum.write_text(f"{archive_sha}  {archive.name}\n")
            package = replace(package, manifest_bytes=manifest_bytes, manifest_sha256=manifest_sha,
                              tag=tag, installer=installer)
            bundle = compose.compose_candidate(source, package)
            output = root / "private-output"
            receipt = compose.prepare_output(bundle, output)

            launcher_sha = hashlib.sha256(bundle.launcher).hexdigest()
            launcher_name = f"install-forgejo-launcher-{launcher_sha[:16]}.sh"
            self.assertEqual((output / launcher_name).read_bytes(), bundle.launcher)
            self.assertEqual((output / "install.sh").read_bytes(), bundle.launcher)
            self.assertEqual(receipt["launcher"], {
                "asset": launcher_name,
                "sha256": launcher_sha,
                "bytes": len(bundle.launcher),
                "bootstrap_tag": bundle.bootstrap_tag,
                "bootstrap_sha256": hashlib.sha256(bundle.bootstrap).hexdigest(),
                "bootstrap_bytes": len(bundle.bootstrap),
            })
            for name in (launcher_name, "install.sh"):
                self.assertEqual(receipt["assets"][name], {
                    "sha256": launcher_sha, "bytes": len(bundle.launcher)})
            self.assertFalse(receipt["production_ready"])
            self.assertFalse(receipt["published"])
            self.assertFalse(receipt["native_acceptance_claimed"])

    def test_generated_launcher_executes_only_verified_saved_bootstrap_and_cleans_up(self):
        payload = (
            "import os,pathlib,sys\n"
            "pathlib.Path(os.environ['BOOTSTRAP_MARKER']).write_text('ran:' + repr(sys.argv[1:]))\n"
        ).encode()
        tag = "nudox-diagnostic-installer-20261011-candidate"
        launcher = compose.candidate_launcher(
            (ROOT / "install_diagnostic_preview.sh").read_bytes(), tag, payload)
        sha = hashlib.sha256(payload).hexdigest()
        expected_url = compose.bootstrap.release_url(
            tag, f"install-forgejo-bootstrap-{sha[:16]}.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            bindir = root / "bin"
            bindir.mkdir()
            tmpdir = root / "private-tmp"
            tmpdir.mkdir(mode=0o700)
            payload_path = root / "payload.py"
            payload_path.write_bytes(payload)
            marker = root / "bootstrap-ran"
            curl_log = root / "curl-args.json"
            curl = bindir / "curl"
            curl.write_text("#!" + sys.executable + "\n" + '''import json,os,pathlib,sys
args=sys.argv[1:]
pathlib.Path(os.environ['CURL_LOG']).write_text(json.dumps(args))
body=pathlib.Path(os.environ['CURL_BODY']).read_bytes()
if os.environ.get('CURL_PARTIAL'): body=body[:max(1,len(body)//2)]
pathlib.Path(args[args.index('--output')+1]).write_bytes(body)
sys.exit(int(os.environ.get('CURL_STATUS','0')))
''')
            curl.chmod(0o755)
            uname = bindir / "uname"
            uname.write_text("#!/bin/sh\ncase \"$1\" in -s) echo Darwin;; -m) echo arm64;; esac\n")
            uname.chmod(0o755)
            env = {
                **os.environ,
                "PATH": str(bindir) + os.pathsep + os.environ.get("PATH", ""),
                "TMPDIR": str(tmpdir),
                "NUDOX_INSTALL_PYTHON": sys.executable,
                "CURL_BODY": str(payload_path),
                "CURL_LOG": str(curl_log),
                "BOOTSTRAP_MARKER": str(marker),
            }
            success = subprocess.run(
                ["/bin/sh", "-s", "--", "argument with spaces"],
                input=launcher, env=env, capture_output=True, timeout=15)
            self.assertEqual(success.returncode, 0, success.stderr.decode("utf-8", "replace"))
            self.assertEqual(marker.read_text(), "ran:['argument with spaces']")
            args = json.loads(curl_log.read_text())
            self.assertEqual(args[0], "-q")
            self.assertEqual(args[args.index("--max-time") + 1], "180")
            self.assertEqual(args[args.index("--connect-timeout") + 1], "15")
            self.assertEqual(args[args.index("--max-filesize") + 1], str(len(payload)))
            for flag in ("--fail", "--proto", "--proto-redir", "--tlsv1.2"):
                self.assertIn(flag, args)
            self.assertEqual(args[args.index("--proto") + 1], "=https")
            self.assertEqual(args[args.index("--proto-redir") + 1], "=https")
            self.assertEqual(args[-1], expected_url)
            self.assertEqual(list(tmpdir.iterdir()), [])

            marker.unlink()
            env["CURL_PARTIAL"] = "1"
            truncated = subprocess.run(["/bin/sh", "-s"], input=launcher, env=env,
                                       capture_output=True, timeout=15)
            self.assertEqual(truncated.returncode, 1, truncated.stderr.decode("utf-8", "replace"))
            self.assertIn(b"size/SHA-256 mismatch", truncated.stderr)
            self.assertFalse(marker.exists())
            self.assertEqual(list(tmpdir.iterdir()), [])

            env.pop("CURL_PARTIAL")
            env["CURL_STATUS"] = "28"
            timed_out = subprocess.run(["/bin/sh", "-s"], input=launcher, env=env,
                                       capture_output=True, timeout=15)
            self.assertEqual(timed_out.returncode, 28, timed_out.stderr.decode("utf-8", "replace"))
            self.assertIn(b"HTTPS download failed", timed_out.stderr)
            self.assertFalse(marker.exists())
            self.assertEqual(list(tmpdir.iterdir()), [])

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
