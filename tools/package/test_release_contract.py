import copy
import io
import hashlib
import json
import os
import plistlib
import sys
import subprocess
import tarfile
import tempfile
import urllib.error
import unittest
import zipfile
from unittest.mock import patch
from pathlib import Path

import release_contract as release
import install_linux as linux_installer


def linux_diagnostic_package(directory, *, extra_members=(), build_changes=None,
                             package_changes=None, pinned_tag=None, executable_changes=None,
                             source_sha=None, source_tree=None, cargo_lock_sha256=None,
                             version="0.2.0", manifest_changes=None):
    root = Path(directory)
    root.mkdir(parents=True, exist_ok=True)
    source = source_sha or "a" * 40
    tree = source_tree or "b" * 40
    lock_sha = cargo_lock_sha256 or "c" * 64
    tag = f"checkpoint-20261011-{source[:10]}-linux-x64"
    executable_bodies = {"backend-cli": b"cli", "backend-mcp": b"mcp", "backend-locald": b"locald"}
    build_object = {
        "schema": "nudox.runtime-build-manifest.v1",
        "source": {"commit": source, "tree": tree, "clean": True},
        "executables": {
            name: {"sha256": hashlib.sha256(body).hexdigest(), "bytes": len(body)}
            for name, body in executable_bodies.items()
        },
        "root_receipt": {
            "schema": "nudox.runtime-artifact-build-receipt.v1",
            "sha256": "d" * 64,
        },
    }
    build_object.update(copy.deepcopy(build_changes or {}))
    archive_executable_bodies = {**executable_bodies, **(executable_changes or {})}
    build = json.dumps(build_object, separators=(",", ":")).encode()
    package_object = {
        "schema": "nudox.linux-portable-package.v1",
        "release_tag": tag,
        "source": copy.deepcopy(build_object["source"]),
        "build_manifest_sha256": hashlib.sha256(build).hexdigest(),
        "required_glibc": "2.39",
        "executables": [
            {
                "name": name,
                "original_sha256": build_object["executables"][name]["sha256"],
                "packaged_path": f"bin/{name}",
                "packaged_sha256": hashlib.sha256(body).hexdigest(),
                "packaged_bytes": len(body),
            }
            for name, body in executable_bodies.items()
        ],
    }
    package_object.update(copy.deepcopy(package_changes or {}))
    package = json.dumps(package_object, separators=(",", ":")).encode()
    members = {
        "nudox-linux-x86_64/bin/backend-cli": (archive_executable_bodies["backend-cli"], 0o755),
        "nudox-linux-x86_64/bin/backend-mcp": (archive_executable_bodies["backend-mcp"], 0o755),
        "nudox-linux-x86_64/bin/backend-locald": (archive_executable_bodies["backend-locald"], 0o755),
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
        for name, kind in extra_members:
            member = tarfile.TarInfo(name)
            member.type = tarfile.DIRTYPE if kind == "dir" else tarfile.REGTYPE
            if kind == "file":
                member.size = 1
                bundle.addfile(member, io.BytesIO(b"x"))
            else:
                bundle.addfile(member)
    manifest = {
        "schema": 1, "version": version, "source_sha": source,
        "source_tree": tree, "cargo_lock_sha256": lock_sha,
        "platform": "linux-x64", "target": "x86_64-unknown-linux-gnu",
        "asset": archive.name, "sha256": release.sha256(archive),
        "size_bytes": archive.stat().st_size, "minimum_glibc": "2.39",
        "build_manifest_sha256": hashlib.sha256(build).hexdigest(),
        "packaging_manifest_sha256": hashlib.sha256(package).hexdigest(),
    }
    manifest.update(copy.deepcopy(manifest_changes or {}))
    manifest_bytes = json.dumps(manifest, separators=(",", ":")).encode()
    (root / "release-manifest.json").write_bytes(manifest_bytes)
    (root / (archive.name + ".sha256")).write_text(f'{manifest["sha256"]}  {archive.name}\n')
    installer_source = (Path(__file__).parent / "install_linux.py").read_bytes()
    selected_tag = pinned_tag if pinned_tag is not None else tag
    pinned_installer = installer_source.replace(b'PINNED_RELEASE_TAG = ""',
                                                 f'PINNED_RELEASE_TAG = "{selected_tag}"'.encode(), 1)
    pinned_installer = pinned_installer.replace(
        b'PINNED_MANIFEST_SHA256 = ""',
        f'PINNED_MANIFEST_SHA256 = "{hashlib.sha256(manifest_bytes).hexdigest()}"'.encode(), 1)
    (root / "install-linux-x64.py").write_bytes(pinned_installer)
    (root / "install-linux-x64-channel.py").write_bytes(installer_source)
    qa = {
        "schema": 1, "platform": "linux-x64",
        "archive_sha256": "replace-with-the-release-manifest-sha256",
        "source_sha": "replace-with-the-release-source-commit",
        "tested_by": "", "tested_at": "", "ubuntu_release": "", "glibc_version": "",
        "cases": {name: False for name in release.LINUX_QA_CASES},
    }
    (root / "native-qa-linux-x64.json").write_text(json.dumps(qa))
    return manifest


def linux_source_checkout(path, *, version="0.2.0"):
    path = Path(path)
    package_tools = path / "tools" / "package"
    package_tools.mkdir(parents=True)
    (path / "Cargo.toml").write_text(
        f'[workspace]\n[workspace.package]\nversion = "{version}"\n')
    lock_bytes = b"# source-validation fixture lock\n"
    (path / "Cargo.lock").write_bytes(lock_bytes)
    (package_tools / "install_linux.py").write_bytes(
        (Path(__file__).parent / "install_linux.py").read_bytes())
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    subprocess.run(["git", "-C", str(path), "add", "Cargo.toml", "Cargo.lock", "tools/package/install_linux.py"], check=True)
    environment = os.environ.copy()
    environment.update({"GIT_AUTHOR_NAME": "Nudox test", "GIT_AUTHOR_EMAIL": "test@example.invalid",
                        "GIT_COMMITTER_NAME": "Nudox test", "GIT_COMMITTER_EMAIL": "test@example.invalid"})
    subprocess.run(["git", "-C", str(path), "commit", "-q", "-m", "synthetic package source"],
                   check=True, env=environment)
    revision = subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"], text=True).strip()
    tree = subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD^{tree}"], text=True).strip()
    return {"root": path, "revision": revision, "tree": tree,
            "lock_sha256": hashlib.sha256(lock_bytes).hexdigest()}


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
        self.tag_reference = {
            "ref": "refs/tags/checkpoint-test",
            "object": {"type": "commit", "sha": "a" * 40},
        }

    def api(self, path, method="GET", body=None):
        self.calls.append((path, method, body))
        if path.startswith("/git/ref/tags/"):
            return self.tag_reference
        if path.startswith("/git/tags/"):
            raise AssertionError("unexpected annotated tag lookup")
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
    def verify_tag_pair(self, github, forgejo_sha="a" * 40):
        tag = "checkpoint-test"
        outputs = [
            "ssh://forgejo@dev.nudox.org:2222/Nudox/Backend.git\n",
            f"{forgejo_sha}\trefs/tags/{tag}\n",
        ]
        with patch.object(release.subprocess, "check_output", side_effect=outputs):
            release.require_published_source_tag(Path("/fixture/source"), tag, "a" * 40, github)

    def test_release_tag_preflight_requires_both_canonical_and_distribution_refs(self):
        absent = FakeGitHub()
        with patch.object(release.subprocess, "check_output", side_effect=[
            "ssh://forgejo@dev.nudox.org:2222/Nudox/Backend.git\n",
            subprocess.CalledProcessError(2, ["git", "ls-remote"]),
        ]):
            with self.assertRaisesRegex(ValueError, "canonical Forgejo tag checkpoint-test is absent"):
                release.require_published_source_tag(Path("/fixture/source"), "checkpoint-test", "a" * 40, absent)
        self.assertEqual(absent.calls, [], "missing canonical refs must be refused before GitHub is queried")

        absent_error = None
        with patch.object(release.subprocess, "check_output", side_effect=[
            "ssh://forgejo@dev.nudox.org:2222/Nudox/Backend.git\n",
            subprocess.CalledProcessError(2, ["git", "ls-remote"]),
        ]):
            with self.assertRaises(ValueError) as caught:
                release.require_published_source_tag(Path("/fixture/source"), "checkpoint-test", "a" * 40, absent)
            absent_error = str(caught.exception)
        self.assertIn("git -C /fixture/source tag checkpoint-test " + "a" * 40, absent_error)
        self.assertIn("git -C /fixture/source push origin refs/tags/checkpoint-test", absent_error)

        foreign_forgejo = FakeGitHub()
        with self.assertRaisesRegex(ValueError, "Forgejo release tag does not resolve"):
            self.verify_tag_pair(foreign_forgejo, "b" * 40)
        self.assertEqual(foreign_forgejo.calls, [], "a foreign canonical tag must be refused before GitHub is queried")

        foreign_github = FakeGitHub()
        foreign_github.tag_reference["object"]["sha"] = "c" * 40
        with self.assertRaisesRegex(ValueError, "GitHub distribution tag does not resolve"):
            self.verify_tag_pair(foreign_github)
        self.assertEqual([call[0] for call in foreign_github.calls], ["/git/ref/tags/checkpoint-test"])

        absent_github = FakeGitHub()

        def missing_reference(path, method="GET", body=None):
            absent_github.calls.append((path, method, body))
            raise urllib.error.HTTPError("https://api.github.com/example", 404, "missing", {}, None)

        with patch.object(absent_github, "api", side_effect=missing_reference), patch.object(
            release.subprocess,
            "check_output",
            side_effect=[
                "ssh://forgejo@dev.nudox.org:2222/Nudox/Backend.git\n",
                "a" * 40 + "\trefs/tags/checkpoint-test\n",
            ],
        ):
            with self.assertRaises(urllib.error.HTTPError):
                release.require_published_source_tag(
                    Path("/fixture/source"), "checkpoint-test", "a" * 40, absent_github
                )
        self.assertEqual([call[0] for call in absent_github.calls], ["/git/ref/tags/checkpoint-test"])

        malformed_github = FakeGitHub()
        malformed_github.tag_reference = {"ref": "refs/tags/checkpoint-test", "object": None}
        with self.assertRaisesRegex(ValueError, "malformed target"):
            self.verify_tag_pair(malformed_github)

        correct = FakeGitHub()
        self.verify_tag_pair(correct)
        self.assertEqual([call[0] for call in correct.calls], ["/git/ref/tags/checkpoint-test"])

    def test_release_tag_preflight_resolves_annotated_github_tag(self):
        github = FakeGitHub()
        github.tag_reference["object"] = {"type": "tag", "sha": "b" * 40}

        def api(path, method="GET", body=None):
            github.calls.append((path, method, body))
            if path == "/git/ref/tags/checkpoint-test":
                return github.tag_reference
            if path == "/git/tags/" + "b" * 40:
                return {"object": {"type": "commit", "sha": "a" * 40}}
            raise AssertionError(f"unexpected GitHub tag API request: {path}")

        with patch.object(github, "api", side_effect=api), patch.object(release.subprocess, "check_output", side_effect=[
            "https://dev.nudox.org/git/Nudox/Backend.git\n",
            (
                "b" * 40 + "\trefs/tags/checkpoint-test\n" +
                "a" * 40 + "\trefs/tags/checkpoint-test^{}\n"
            ),
        ]):
            release.require_published_source_tag(Path("/fixture/source"), "checkpoint-test", "a" * 40, github)
        self.assertEqual([call[0] for call in github.calls], [
            "/git/ref/tags/checkpoint-test", "/git/tags/" + "b" * 40,
        ])

    def test_release_tag_preflight_rejects_noncanonical_origins_and_duplicates(self):
        github = FakeGitHub()
        for origin in (
            "https://dev.nudox.org/Nudox/Backend.git",
            "https://dev.nudox.org/git/SomeoneElse/Backend.git",
            "https://dev.nudox.org/git/Nudox/Backend.git?redirect=https://github.com/",
            "https://dev.nudox.org/git/Nudox/Backend.git#fragment",
            "https://user@dev.nudox.org/git/Nudox/Backend.git",
            "https://dev.nudox.org.attacker.invalid/git/Nudox/Backend.git",
            "ssh://forgejo@dev.nudox.org:22/Nudox/Backend.git",
        ):
            with self.subTest(origin=origin), patch.object(
                release.subprocess, "check_output", return_value=origin + "\n"
            ):
                with self.assertRaisesRegex(ValueError, "origin is not the canonical"):
                    release.require_published_source_tag(
                        Path("/fixture/source"), "checkpoint-test", "a" * 40, github
                    )
        self.assertEqual(github.calls, [])

    def test_canonical_forgejo_origin_parser_accepts_only_known_clone_forms(self):
        for origin in (
            "https://dev.nudox.org/git/Nudox/Backend.git",
            "ssh://forgejo@dev.nudox.org:2222/Nudox/Backend.git",
            "forgejo@dev.nudox.org:Nudox/Backend.git",
        ):
            with self.subTest(origin=origin):
                self.assertTrue(release._is_canonical_forgejo_origin(origin))
        for origin in (
            "https://dev.nudox.org/git/Nudox/Backend.git?",
            "https://dev.nudox.org/git/Nudox/Backend.git#",
            "git@dev.nudox.org:Nudox/Backend.git",
        ):
            with self.subTest(origin=origin):
                self.assertFalse(release._is_canonical_forgejo_origin(origin))

        github = FakeGitHub()
        duplicated = ("a" * 40 + "\trefs/tags/checkpoint-test\n") * 2
        with patch.object(release.subprocess, "check_output", side_effect=[
            "https://dev.nudox.org/git/Nudox/Backend.git\n", duplicated,
        ]):
            with self.assertRaisesRegex(ValueError, "duplicate refs"):
                release.require_published_source_tag(
                    Path("/fixture/source"), "checkpoint-test", "a" * 40, github
                )
        self.assertEqual(github.calls, [])

    def test_release_tag_preflight_bounds_git_remote_lookup(self):
        github = FakeGitHub()
        with patch.object(
            release.subprocess,
            "check_output",
            side_effect=subprocess.TimeoutExpired(["git", "remote"], 30),
        ) as check_output:
            with self.assertRaisesRegex(ValueError, "failed or exceeded 30 seconds"):
                release.require_published_source_tag(
                    Path("/fixture/source"), "checkpoint-test", "a" * 40, github
                )
        self.assertEqual(check_output.call_args.kwargs["timeout"], 30)
        self.assertEqual(github.calls, [])

        with patch.object(release.subprocess, "check_output", side_effect=[
            "https://dev.nudox.org/git/Nudox/Backend.git\n",
            subprocess.TimeoutExpired(["git", "ls-remote"], 30),
        ]) as check_output:
            with self.assertRaisesRegex(ValueError, "tag lookup exceeded 30 seconds"):
                release.require_published_source_tag(
                    Path("/fixture/source"), "checkpoint-test", "a" * 40, github
                )
        self.assertEqual(check_output.call_args.kwargs["timeout"], 30)
        self.assertEqual(github.calls, [])

    def test_preflight_failure_does_not_mutate_existing_release(self):
        github = FakeGitHub(existing={
            "id": 99,
            "tag_name": "v0.2.0-rc.1-macos",
            "assets": [{"name": "existing-asset", "id": 1}],
            "draft": True,
            "prerelease": True,
            "body": "existing draft",
        })
        with tempfile.TemporaryDirectory() as directory, patch.object(
            release, "validate", return_value={"version": "0.2.0", "source_sha": "a" * 40, "sha256": "b" * 64, "platform": "macos"}
        ), patch.object(release, "check_fast"), patch.object(
            release, "GitHub", return_value=github
        ), patch.object(
            release, "require_published_source_tag", side_effect=ValueError("tag preflight refused")
        ), patch.object(sys, "argv", [
            "release_contract.py", "candidate", "--candidate", directory,
            "--source", "/fixture/source", "--candidate-tag", "v0.2.0-rc.1-macos",
        ]):
            with self.assertRaisesRegex(ValueError, "tag preflight refused"):
                release.main()
        self.assertEqual(github.calls, [], "failed tag preflight must precede release lookup, POST, and PATCH")

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

    def test_linux_archive_raw_names_match_installer_policy(self):
        for raw_name in (
            "nudox-linux-x86_64//escaped",
            "nudox-linux-x86_64/./escaped",
            "nudox-linux-x86_64/../escaped",
        ):
            with self.subTest(raw_name=raw_name), tempfile.TemporaryDirectory() as directory:
                manifest = linux_diagnostic_package(directory, extra_members=((raw_name, "file"),))
                with self.assertRaisesRegex(ValueError, "noncanonical or unsafe"):
                    release.validate_linux(Path(directory), manifest, require_qa=False)

                member = tarfile.TarInfo(raw_name)
                member.size = 1
                with self.assertRaises(linux_installer.InstallError):
                    linux_installer._safe_member(member, "nudox-linux-x86_64")

    def test_linux_archive_allows_exact_root_and_rejects_normalized_duplicates(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = linux_diagnostic_package(directory)
            self.assertEqual(release.validate_linux(Path(directory), manifest, require_qa=False), manifest)

        with tempfile.TemporaryDirectory() as directory:
            manifest = linux_diagnostic_package(directory, extra_members=(
                ("nudox-linux-x86_64/extra/", "dir"),
                ("nudox-linux-x86_64/extra", "dir"),
            ))
            with self.assertRaisesRegex(ValueError, "duplicate normalized paths"):
                release.validate_linux(Path(directory), manifest, require_qa=False)

    def test_linux_archive_rejects_file_parent_collisions_and_unbounded_headers(self):
        with tempfile.TemporaryDirectory() as directory:
            manifest = linux_diagnostic_package(
                directory, extra_members=(("nudox-linux-x86_64/bin", "file"),))
            with self.assertRaisesRegex(ValueError, "file used as a parent"):
                release.validate_linux(Path(directory), manifest, require_qa=False)

        members = []
        root = tarfile.TarInfo("nudox-linux-x86_64")
        root.type = tarfile.DIRTYPE
        members.append(root)
        for name in (
            "bin/backend-cli", "bin/backend-mcp", "bin/backend-locald",
            "build-manifest.json", "packaging-manifest.json", "README.txt",
        ):
            member = tarfile.TarInfo("nudox-linux-x86_64/" + name)
            member.size = 1
            members.append(member)
        for name, size in (("large-a", release.LINUX_MAX_MEMBER_BYTES),
                           ("large-b", release.LINUX_MAX_MEMBER_BYTES),
                           ("large-c", 1)):
            member = tarfile.TarInfo("nudox-linux-x86_64/" + name)
            member.size = size
            members.append(member)
        with self.assertRaisesRegex(ValueError, "uncompressed size exceeds"):
            release.validate_linux_archive_members(members)

        oversized_member = tarfile.TarInfo("nudox-linux-x86_64/oversized")
        oversized_member.size = release.LINUX_MAX_MEMBER_BYTES + 1
        with self.assertRaisesRegex(ValueError, "member exceeds the safety limit"):
            release.validate_linux_archive_members([root, oversized_member])
        with self.assertRaisesRegex(ValueError, "empty or excessive member list"):
            release.validate_linux_archive_members([root] * (release.LINUX_MAX_ARCHIVE_MEMBERS + 1))

    def test_linux_validation_stops_reading_headers_at_the_member_cap(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = linux_diagnostic_package(root)

            class LongTar:
                consumed = 0

                def __enter__(self):
                    return self

                def __exit__(self, *args):
                    return False

                def __iter__(self):
                    while True:
                        self.consumed += 1
                        yield tarfile.TarInfo(f"nudox-linux-x86_64/member-{self.consumed}")

            archive = LongTar()
            with patch.object(release.tarfile, "open", return_value=archive):
                with self.assertRaisesRegex(ValueError, "too many members"):
                    release.validate_linux(root, manifest, require_qa=False)
            self.assertEqual(archive.consumed, release.LINUX_MAX_ARCHIVE_MEMBERS + 1)

    def test_linux_member_stream_rejects_size_limits_before_reading_more_headers(self):
        class MemberStream:
            def __init__(self, members):
                self.members = iter(members)
                self.consumed = 0

            def __iter__(self):
                for member in self.members:
                    self.consumed += 1
                    yield member

        oversized = tarfile.TarInfo("nudox-linux-x86_64/oversized")
        oversized.size = release.LINUX_MAX_MEMBER_BYTES + 1
        stream = MemberStream([oversized])
        with self.assertRaisesRegex(ValueError, "member exceeds the safety limit"):
            release.read_bounded_linux_members(stream)
        self.assertEqual(stream.consumed, 1)

        aggregate_members = []
        for name, size in (("large-a", release.LINUX_MAX_MEMBER_BYTES),
                           ("large-b", release.LINUX_MAX_MEMBER_BYTES),
                           ("large-c", 1), ("must-not-read", 1)):
            member = tarfile.TarInfo("nudox-linux-x86_64/" + name)
            member.size = size
            aggregate_members.append(member)
        stream = MemberStream(aggregate_members)
        with self.assertRaisesRegex(ValueError, "uncompressed size exceeds"):
            release.read_bounded_linux_members(stream)
        self.assertEqual(stream.consumed, 3)

    def test_linux_build_and_packaging_identities_are_cross_bound(self):
        invalid_cases = (
            ("dirty build", {"build_changes": {"source": {
                "commit": "a" * 40, "tree": "b" * 40, "clean": False}}},
             "commit/tree/clean identity"),
            ("different build commit", {"build_changes": {"source": {
                "commit": "f" * 40, "tree": "b" * 40, "clean": True}}},
             "commit/tree/clean identity"),
            ("different build tree", {"build_changes": {"source": {
                "commit": "a" * 40, "tree": "c" * 40, "clean": True}}},
             "commit/tree/clean identity"),
            ("foreign package source", {"package_changes": {"source": {
                "commit": "f" * 40, "tree": "b" * 40, "clean": True}}},
             "source differs from the build"),
            ("wrong package build digest", {"package_changes": {
                "build_manifest_sha256": "e" * 64}}, "exact public build manifest"),
            ("glibc floor mismatch", {"package_changes": {
                "required_glibc": "2.40"}}, "glibc requirement differs"),
            ("package tag differs from pinned installer", {"package_changes": {
                "release_tag": "checkpoint-20261010-aaaaaaaaaa-linux-x64"}},
             "release tag differs from its pinned installer"),
            ("executable differs from build record", {"executable_changes": {
                "backend-cli": b"changed cli bytes"}},
             "archived backend-cli bytes differ"),
        )
        for label, kwargs, message in invalid_cases:
            with self.subTest(label=label), tempfile.TemporaryDirectory() as directory:
                manifest = linux_diagnostic_package(directory, **kwargs)
                with self.assertRaisesRegex(ValueError, message):
                    release.validate_linux(Path(directory), manifest, require_qa=False)

    def test_linux_manifest_digest_is_exact_on_disk_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = linux_diagnostic_package(root)
            admitted_digest = hashlib.sha256((root / "release-manifest.json").read_bytes()).hexdigest()
            (root / "release-manifest.json").write_text(json.dumps(manifest, indent=2))
            with self.assertRaisesRegex(ValueError, "manifest bytes changed after admission"):
                release.validate_linux(root, manifest, require_qa=False,
                                       manifest_sha256=admitted_digest)

    def test_linux_source_lock_version_and_tree_match_the_selected_git_revision(self):
        scenarios = (
            ("matching source", {}, None),
            ("different lock", {"manifest_changes": {"cargo_lock_sha256": "e" * 64}},
             "lock differs from selected canonical source"),
            ("different version", {"manifest_changes": {"version": "0.3.0"}},
             "version/tree differs from selected canonical source"),
            ("different source tree", {"source_tree": "f" * 40},
             "version/tree differs from selected canonical source"),
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = linux_source_checkout(root / "source")
            for label, changes, error in scenarios:
                with self.subTest(label=label):
                    package_dir = root / ("package-" + label.replace(" ", "-"))
                    package_tree = changes.get("source_tree", source["tree"])
                    package_changes = {key: value for key, value in changes.items() if key != "source_tree"}
                    manifest = linux_diagnostic_package(
                        package_dir,
                        source_sha=source["revision"],
                        source_tree=package_tree,
                        cargo_lock_sha256=source["lock_sha256"],
                        **package_changes,
                    )
                    if error is None:
                        self.assertEqual(
                            release.validate_linux(package_dir, manifest, source["root"], require_qa=False),
                            manifest,
                        )
                    else:
                        with self.assertRaisesRegex(ValueError, error):
                            release.validate_linux(package_dir, manifest, source["root"], require_qa=False)

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

    def test_linux_publication_includes_checkpoint_or_channel_installer_under_stable_asset_name(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = {
                "schema": 1, "version": "0.2.0", "source_sha": "a" * 40,
                "source_tree": "b" * 40, "cargo_lock_sha256": "c" * 64,
                "platform": "linux-x64", "target": "x86_64-unknown-linux-gnu",
                "asset": "nudox-linux-x86_64-aaaaaaaaaa.tar.gz", "sha256": "d" * 64,
            }
            (root / manifest["asset"]).write_bytes(b"fixture archive bytes")
            (root / (manifest["asset"] + ".sha256")).write_text("fixture checksum\n")
            (root / "release-manifest.json").write_text("{}")
            (root / "native-qa-linux-x64.json").write_text("{}")
            (root / "install-linux-x64.py").write_bytes(b"pinned installer")
            (root / "install-linux-x64-channel.py").write_bytes(b"channel installer")

            github = FakeGitHub()
            with patch.object(release, "validate", return_value=manifest):
                github.publish(root, "v0.2.0-rc.1-linux-x64", False, "linux-x64", draft_only=True)
            self.assertEqual(github.asset_data[4], b"pinned installer")
            self.assertEqual(github.release["assets"][-1]["name"], "install-linux-x64.py")

            stable_github = FakeGitHub()
            with patch.object(release, "validate", return_value=manifest):
                stable_github.publish(root, "v0.2.0", True, "linux-x64")
            self.assertEqual(stable_github.asset_data[5], b"channel installer")
            self.assertEqual(stable_github.release["assets"][-1]["name"], "install-linux-x64.py")

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
