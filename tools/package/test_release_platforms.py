import hashlib
import io
import json
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch

import release_contract as release
from test_release_contract import FakeGitHub, candidate


def native_candidate(directory, platform):
    root = Path(directory)
    profile = release.PLATFORMS[platform]
    build = json.dumps({"source": {"git_revision": "a" * 40, "git_tree": "b" * 40, "cargo_lock_sha256": "c" * 64}}).encode()
    files = {name: b"native executable fixture" for name in profile["executables"]}
    files[profile["build"]] = build
    archive = root / profile["asset"]
    if platform == "windows":
        with zipfile.ZipFile(archive, "w") as bundle:
            for name, data in files.items():
                bundle.writestr(name, data)
    else:
        with tarfile.open(archive, "w:gz") as bundle:
            for name, data in files.items():
                member = tarfile.TarInfo(name)
                member.size = len(data)
                member.mode = 0o755 if "/bin/" in name else 0o644
                bundle.addfile(member, io.BytesIO(data))
    manifest = {"schema": 1, "version": "0.2.0", "source_sha": "a" * 40, "source_tree": "b" * 40,
                "cargo_lock_sha256": "c" * 64, "platform": platform, "target": profile["target"],
                "asset": archive.name, "sha256": release.sha256(archive), "size_bytes": archive.stat().st_size,
                "minimum_os": "Windows 10" if platform == "windows" else "glibc 2.31",
                "build_manifest_sha256": hashlib.sha256(build).hexdigest(),
                "signed": profile["signed"], "notarized": profile["notarized"]}
    qa = {"schema": 1, "archive_sha256": manifest["sha256"], "source_sha": manifest["source_sha"],
          "tested_by": "native tester", "tested_at": "2026-10-05T20:00:00Z", "os_version": "native OS version",
          "host_os": profile["host"], "target": profile["target"], "cases": {name: True for name in profile["qa"]}}
    (root / "release-manifest.json").write_text(json.dumps(manifest))
    (root / "native-qa.json").write_text(json.dumps(qa))
    (root / (archive.name + ".sha256")).write_text(f'{manifest["sha256"]}  {archive.name}\n')
    return manifest


class PlatformReleaseTests(unittest.TestCase):
    def test_private_staging_precedes_native_qa_and_submit_completes_same_draft(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate(root)
            qa_path = root / "native-qa.json"
            qa = qa_path.read_bytes()
            qa_path.unlink()
            github = FakeGitHub()
            github.publish(root, "v0.2.0-rc.1-macos", False, draft_only=True)
            self.assertTrue(github.release["draft"])
            self.assertEqual(len(github.uploaded), 3)
            with self.assertRaisesRegex(ValueError, "regular file"):
                github.publish(root, "v0.2.0-rc.1-macos", False)
            self.assertTrue(github.release["draft"])
            qa_path.write_bytes(qa)
            github.publish(root, "v0.2.0-rc.1-macos", False)
            self.assertFalse(github.release["draft"])
            self.assertEqual(len(github.uploaded), 4)
            self.assertEqual(len([call for call in github.calls if call[1] == "POST"]), 1)

    def test_ci_uses_latest_verdict_for_the_selected_platform(self):
        statuses = [{"id": 1, "context": "concourse/backend-fast", "status": "success"},
                    {"id": 2, "context": "concourse/backend-windows-wine", "status": "success"},
                    {"id": 3, "context": "concourse/backend-windows-wine", "status": "failure"}]
        with patch.dict(release.os.environ, {"FORGEJO_TOKEN": "test-only"}), patch.object(release.urllib.request, "urlopen", side_effect=lambda *args, **kwargs: io.BytesIO(json.dumps(statuses).encode())):
            release.check_fast("a" * 40, "macos")
            with self.assertRaisesRegex(ValueError, "windows-wine"):
                release.check_fast("a" * 40, "windows")
            with self.assertRaisesRegex(ValueError, "backend-linux"):
                release.check_fast("a" * 40, "linux-x64")

    def test_publication_requires_deployed_versioned_download_route(self):
        class Response(io.BytesIO):
            headers = {}
        with patch.object(release.urllib.request, "urlopen", return_value=Response(b'{}')):
            with self.assertRaisesRegex(ValueError, "versioned Auth download route"):
                release.public_metadata(require_versioned=True)

    def test_all_native_platforms_require_own_target_signing_and_qa(self):
        for platform in ("linux-x64", "linux-arm64", "windows"):
            with self.subTest(platform=platform), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                manifest = native_candidate(root, platform)
                self.assertEqual(release.validate(root, platform=platform), manifest)
                with self.assertRaisesRegex(ValueError, "wrong platform"):
                    release.validate(root, platform="macos")
                qa = json.loads((root / "native-qa.json").read_text())
                qa["host_os"] = "emulated"
                (root / "native-qa.json").write_text(json.dumps(qa))
                with self.assertRaisesRegex(ValueError, "wrong OS/architecture"):
                    release.validate(root)
                native_candidate(root, platform)
                if platform == "windows":
                    manifest["signed"] = False
                    (root / "release-manifest.json").write_text(json.dumps(manifest))
                    with self.assertRaisesRegex(ValueError, "signing/notarization"):
                        release.validate(root)

    def test_platforms_append_to_stable_release_without_metadata_collision(self):
        github = FakeGitHub()
        for platform in ("macos", "linux-x64", "linux-arm64", "windows"):
            with tempfile.TemporaryDirectory() as directory:
                candidate(directory) if platform == "macos" else native_candidate(directory, platform)
                github.publish(directory, "v0.2.0", True)
                for kind in ("release-manifest", "native-qa"):
                    path = Path(directory) / (kind + ".json")
                    path.rename(Path(directory) / f"{kind}-{platform}.json")
                self.assertEqual(release.validate(directory, platform=platform)["platform"], platform)
                github.publish(directory, "v0.2.0", True, platform)
        self.assertEqual(len(github.uploaded), 16)
        self.assertEqual(len({asset["name"] for asset in github.release["assets"]}), 16)

    def test_same_version_cannot_mix_source_revisions(self):
        github = FakeGitHub()
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            candidate(first)
            github.publish(first, "v0.2.0", True)
            native_candidate(second, "windows")
            existing = next(asset for asset in github.release["assets"] if asset["name"] == "release-manifest-macos.json")
            data = json.loads(github.asset_data[existing["id"]])
            data["source_sha"] = "d" * 40
            github.asset_data[existing["id"]] = json.dumps(data).encode()
            with self.assertRaisesRegex(ValueError, "same source revision"):
                github.publish(second, "v0.2.0", True)
            self.assertEqual(len(github.uploaded), 4)

    def test_mac_browser_and_cli_acceptance_cannot_be_skipped(self):
        for missing in ("browser_download_launch", "cli_mcp"):
            with tempfile.TemporaryDirectory() as directory:
                candidate(directory)
                path = Path(directory) / "native-qa.json"
                qa = json.loads(path.read_text())
                del qa["cases"][missing]
                path.write_text(json.dumps(qa))
                with self.assertRaisesRegex(ValueError, "missing or failed cases"):
                    release.validate(directory)

    def test_homebrew_uses_public_versioned_url_and_refuses_stale_release(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = candidate(directory)
            cask = release.homebrew_cask(manifest)
            self.assertIn(release.versioned_download_url(manifest), cask)
            self.assertIn('sha256 "' + manifest["sha256"] + '"', cask)
            self.assertIn("depends_on macos: :sonoma", cask)
            self.assertNotIn("github.com/nudoxorg/Backend/releases/download", cask)
            with patch.object(release, "public_metadata", return_value={"platforms": {"macos": {}}}), patch.object(release, "GitHub") as github:
                with self.assertRaisesRegex(ValueError, "current promoted release"):
                    release.update_homebrew(manifest)
                github.assert_not_called()

    def test_unsafe_linux_archive_is_rejected_before_reading_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = native_candidate(root, "linux-x64")
            archive = root / manifest["asset"]
            with tarfile.open(archive, "w:gz") as bundle:
                member = tarfile.TarInfo("Nudox/bin/backend-cli")
                member.type = tarfile.SYMTYPE
                member.linkname = "/nix/store/build-machine-only/backend-cli"
                bundle.addfile(member)
            manifest.update(sha256=release.sha256(archive), size_bytes=archive.stat().st_size)
            (root / "release-manifest.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "links or special"):
                release.validate(root)


if __name__ == "__main__":
    unittest.main()
