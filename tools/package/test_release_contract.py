import io
import hashlib
import json
import plistlib
import tarfile
import tempfile
import urllib.error
import unittest
import zipfile
from unittest.mock import patch
from pathlib import Path

import release_contract as release


def linux_diagnostic_package(directory):
    root = Path(directory)
    source = "a" * 40
    build = json.dumps({"source": {"commit": source}}).encode()
    package = json.dumps({"schema": "nudox.linux-portable-package.v1",
                          "release_tag": f"checkpoint-20261011-{source[:10]}-linux-x64"}).encode()
    members = {
        "nudox-linux-x86_64/bin/backend-cli": (b"cli", 0o755),
        "nudox-linux-x86_64/bin/backend-mcp": (b"mcp", 0o755),
        "nudox-linux-x86_64/bin/backend-locald": (b"locald", 0o755),
        "nudox-linux-x86_64/build-manifest.json": (build, 0o644),
        "nudox-linux-x86_64/packaging-manifest.json": (package, 0o644),
        "nudox-linux-x86_64/README.txt": (b"synthetic diagnostic package", 0o644),
    }
    archive = root / f"nudox-linux-x86_64-{source[:10]}.tar.gz"
    with tarfile.open(archive, "w:gz") as bundle:
        root_member = tarfile.TarInfo("nudox-linux-x86_64")
        root_member.type = tarfile.DIRTYPE
        bundle.addfile(root_member)
        for name, (body, mode) in members.items():
            member = tarfile.TarInfo(name)
            member.size = len(body)
            member.mode = mode
            bundle.addfile(member, io.BytesIO(body))
    manifest = {
        "schema": 1, "version": "0.2.0", "source_sha": source,
        "source_tree": "b" * 40, "cargo_lock_sha256": "c" * 64,
        "platform": "linux-x64", "target": "x86_64-unknown-linux-gnu",
        "asset": archive.name, "sha256": release.sha256(archive),
        "size_bytes": archive.stat().st_size, "minimum_glibc": "2.39",
        "build_manifest_sha256": hashlib.sha256(build).hexdigest(),
        "packaging_manifest_sha256": hashlib.sha256(package).hexdigest(),
    }
    (root / "release-manifest.json").write_text(json.dumps(manifest))
    (root / (archive.name + ".sha256")).write_text(f'{manifest["sha256"]}  {archive.name}\n')
    (root / "install-linux-x64.py").write_text("# synthetic pinned installer\n")
    (root / "install-linux-x64-channel.py").write_text("# synthetic channel installer\n")
    qa = {
        "schema": 1, "platform": "linux-x64",
        "archive_sha256": "replace-with-the-release-manifest-sha256",
        "source_sha": "replace-with-the-release-source-commit",
        "tested_by": "", "tested_at": "", "ubuntu_release": "", "glibc_version": "",
        "cases": {name: False for name in release.LINUX_QA_CASES},
    }
    (root / "native-qa-linux-x64.json").write_text(json.dumps(qa))
    return manifest


def candidate(directory):
    root = Path(directory)
    build = json.dumps({"source": {"git_revision": "a" * 40, "git_tree": "b" * 40, "cargo_lock_sha256": "c" * 64}}).encode()
    with zipfile.ZipFile(root / release.ASSET, "w") as archive:
        for executable in ("Nudox", "nudox-cli", "nudox-mcp", "nudox-locald", "backend-desktop", "backend-cli", "backend-mcp", "backend-locald"):
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
        self.asset_data = {}
        self.calls = []

    def api(self, path, method="GET", body=None):
        self.calls.append((path, method, body))
        if method == "POST":
            self.release = {"id": 42, "tag_name": body["tag_name"], "assets": [], "draft": True, "prerelease": body["prerelease"], "body": body["body"]}
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
        self.asset_data[asset["id"]] = path.read_bytes()
        self.release["assets"].append(asset)
        return asset

    def asset_json(self, asset):
        return json.loads(self.asset_data[asset["id"]])

    def asset_hash(self, asset):
        return self.uploaded.get(asset["id"], "different bytes")


class CandidateTests(unittest.TestCase):
    def test_diagnostic_linux_validation_checks_package_without_claiming_native_qa(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = linux_diagnostic_package(directory)
            self.assertEqual(release.validate_linux(Path(directory), manifest, require_qa=False), manifest)
            with self.assertRaisesRegex(ValueError, "native QA does not attest"):
                release.validate_linux(Path(directory), manifest)
            archive = Path(directory) / manifest["asset"]
            with archive.open("ab") as stream:
                stream.write(b"corruption")
            with self.assertRaisesRegex(ValueError, "archive differs from its immutable manifest"):
                release.validate_linux(Path(directory), manifest, require_qa=False)

    def test_missing_release_lookup_closes_http_response(self):
        github = FakeGitHub()
        missing = urllib.error.HTTPError("https://api.example/releases/tag", 404, "missing", {}, io.BytesIO(b"not found"))

        def api(path, method="GET", body=None):
            if path.startswith("/releases/tags/"):
                raise missing
            if path.startswith("/releases?"):
                return []
            raise AssertionError(f"unexpected API request: {path}")

        with patch.object(github, "api", side_effect=api):
            self.assertIsNone(github.find_release("checkpoint-test"))

        self.assertTrue(missing.fp.closed)

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
            github.publish(directory, "v0.2.0-rc.1-macos", False)
            # A retry after interruption before the final draft publication.
            github.release["draft"] = True
            github.publish(directory, "v0.2.0-rc.1-macos", False)
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
            with self.assertRaisesRegex(ValueError, "previous platform channel restored"):
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
                github.publish(root, "v0.2.0-rc.1-macos", False)
            self.assertEqual(github.calls, [])


if __name__ == "__main__":
    unittest.main()
