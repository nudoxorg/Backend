import hashlib
import json
import plistlib
import tempfile
import unittest
import zipfile
from unittest.mock import patch
from pathlib import Path

import release_contract as release


def candidate(directory):
    root = Path(directory)
    build = json.dumps({"source": {"git_revision": "a" * 40, "git_tree": "b" * 40, "cargo_lock_sha256": "c" * 64}}).encode()
    with zipfile.ZipFile(root / release.ASSET, "w") as archive:
        for executable in ("Nudox", "backend-desktop", "backend-cli", "backend-mcp", "backend-locald"):
            archive.writestr("Nudox.app/Contents/MacOS/" + executable, b"test fixture")
        archive.writestr("Nudox.app/Contents/Resources/build-manifest.json", build)
        archive.writestr("Nudox.app/Contents/Info.plist", plistlib.dumps({"CFBundleShortVersionString": "0.2.0", "LSMinimumSystemVersion": "14.0"}))
    manifest = {"schema": 1, "version": "0.2.0", "source_sha": "a" * 40, "source_tree": "b" * 40, "cargo_lock_sha256": "c" * 64, "platform": "macos", "target": "aarch64-apple-darwin", "asset": release.ASSET, "sha256": release.sha256(root / release.ASSET), "size_bytes": (root / release.ASSET).stat().st_size, "minimum_os": "14.0", "build_manifest_sha256": hashlib.sha256(build).hexdigest(), "signed": True, "notarized": True}
    (root / "release-manifest.json").write_text(json.dumps(manifest))
    (root / (release.ASSET + ".sha256")).write_text(f'{manifest["sha256"]}  {release.ASSET}\n')
    qa = {"schema": 1, "archive_sha256": manifest["sha256"], "source_sha": manifest["source_sha"], "tested_by": "test operator", "tested_at": "2026-10-05T20:00:00Z", "macos_version": "14.0", "cases": {name: True for name in release.QA_CASES}}
    (root / "native-qa.json").write_text(json.dumps(qa))
    return manifest


class FakeGitHub(release.GitHub):
    def __init__(self, existing=None):
        super().__init__("acme/test", "test-only-token")
        self.release = existing
        self.uploaded = {}
        self.calls = []

    def api(self, path, method="GET", body=None):
        self.calls.append((path, method, body))
        if method == "POST":
            self.release = {"id": 42, "tag_name": body["tag_name"], "assets": [], "draft": True, "prerelease": body["prerelease"]}
        elif method == "PATCH":
            self.release.update(body)
        elif path.startswith("/releases?"):
            return [self.release] if self.release is not None else []
        elif self.release is None or self.release["draft"]:
            import urllib.error
            raise urllib.error.HTTPError("https://api.github.com/example", 404, "missing", {}, None)
        return self.release

    def upload(self, release_id, path):
        asset = {"name": path.name, "id": len(self.uploaded) + 1}
        self.uploaded[asset["id"]] = release.sha256(path)
        self.release["assets"].append(asset)
        return asset

    def asset_hash(self, asset):
        return self.uploaded.get(asset["id"], "different bytes")


class CandidateTests(unittest.TestCase):
    def test_browser_download_launch_is_required_before_staging_or_publication(self):
        for stable in (False, True):
            for result in (None, False):
                with self.subTest(stable=stable, result=result), tempfile.TemporaryDirectory() as directory:
                    root = Path(directory)
                    candidate(root)
                    qa = json.loads((root / "native-qa.json").read_text())
                    if result is None:
                        qa["cases"].pop("browser_download_launch")
                    else:
                        qa["cases"]["browser_download_launch"] = result
                    (root / "native-qa.json").write_text(json.dumps(qa))
                    github = FakeGitHub()
                    with self.assertRaisesRegex(ValueError, "missing or failed cases"):
                        github.publish(root, "v0.2.0" if stable else "v0.2.0-rc.1", stable)
                    self.assertEqual(github.calls, [])
                    self.assertEqual(github.uploaded, {})

    def test_native_acceptance_and_archive_bytes_must_match(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = candidate(root)
            self.assertEqual(release.validate(root), manifest)
            qa = json.loads((root / "native-qa.json").read_text())
            qa["cases"]["cold_restart"] = False
            (root / "native-qa.json").write_text(json.dumps(qa))
            with self.assertRaisesRegex(ValueError, "failed cases"):
                release.validate(root)
            candidate(root)
            with (root / release.ASSET).open("ab") as stream:
                stream.write(b"modified after QA")
            with self.assertRaisesRegex(ValueError, "immutable manifest"):
                release.validate(root)

    def test_publication_is_read_back_verified_and_retry_does_not_replace_assets(self):
        with tempfile.TemporaryDirectory() as directory:
            candidate(directory)
            github = FakeGitHub()
            github.publish(directory, "v0.2.0", True)
            self.assertFalse(github.release["draft"])
            self.assertEqual(len(github.uploaded), 4)
            self.assertEqual(github.calls[-1][2]["make_latest"], "false")
            github.publish(directory, "v0.2.0", True)
            self.assertEqual(len(github.uploaded), 4)
            github.uploaded[1] = "d" * 64
            with self.assertRaisesRegex(ValueError, "overwrite immutable"):
                github.publish(directory, "v0.2.0", True)

    def test_candidate_retry_finds_draft_by_listing_and_never_creates_duplicate(self):
        with tempfile.TemporaryDirectory() as directory:
            candidate(directory)
            github = FakeGitHub()
            github.publish(directory, "v0.2.0-rc.1", False)
            # A retry after interruption before the final draft publication.
            github.release["draft"] = True
            github.publish(directory, "v0.2.0-rc.1", False)
            self.assertFalse(github.release["draft"])
            self.assertTrue(github.release["prerelease"])
            self.assertEqual(len(github.uploaded), 4)
            self.assertEqual(len([call for call in github.calls if call[1] == "POST"]), 1)
            self.assertTrue(any(call[0].startswith("/releases?") for call in github.calls))

    def test_failed_public_download_rolls_back_with_compare_and_swap(self):
        old = {"schema": 1, "platforms": {"macos": {"version": "0.1.0"}, "windows": {"version": "0.1.0"}}}
        updated = {"schema": 1, "platforms": {**old["platforms"], "macos": {"version": "0.2.0"}}}
        replies = [{"sha256": "a" * 64, "catalog": old}, {"sha256": "b" * 64, "catalog": updated}, {"sha256": "c" * 64, "catalog": old}]
        commands = []
        def ssh(command, **kwargs):
            commands.append(json.loads(kwargs["input"]))
            return type("Result", (), {"stdout": json.dumps(replies.pop(0))})()
        manifest = {"version": "0.2.0", "source_sha": "d" * 40, "asset": release.ASSET, "sha256": "e" * 64, "minimum_os": "14.0", "platform": "macos"}
        with patch.object(release.subprocess, "run", side_effect=ssh), patch.object(release, "public_metadata", return_value=old), patch.object(release, "verify_public_download", side_effect=ValueError("bad bytes")):
            with self.assertRaisesRegex(ValueError, "previous Mac channel restored"):
                release.promote(manifest, "test", "key", "hosts")
        self.assertEqual(commands[-1], {"action": "rollback", "expected_sha256": "b" * 64, "platform": "macos", "history_sha256": "a" * 64})

    def test_missing_qa_and_invalid_candidate_tag_never_publish(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            candidate(root)
            github = FakeGitHub()
            with self.assertRaisesRegex(ValueError, "candidate tag"):
                github.publish(root, "v0.2.0", False)
            self.assertEqual(github.calls, [])
            (root / "native-qa.json").unlink()
            with self.assertRaisesRegex(ValueError, "regular file"):
                github.publish(root, "v0.2.0-rc.1", False)
            self.assertEqual(github.calls, [])


if __name__ == "__main__":
    unittest.main()
