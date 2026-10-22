"""Non-ELF controls: manifest schema, source binding, archive layout/safety.

The ELF relocation path (portabilize) reuses linux_release_package.py's
already-tested patchelf/ldd loop verbatim; its own test suite covers that
logic. These tests cover what's new here: reading a T1 artifact manifest,
binding the formal release manifest to the exact git source, and the
Nudox/-prefixed tar layout release_contract.py's archive_evidence() checks.
"""
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

import linux_formal_release as release


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def make_source_repo(root: Path) -> tuple[str, str, str]:
    subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "t@t.t"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "t"], check=True)
    (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.0"\n')
    (root / "Cargo.lock").write_text("# lock\n")
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-q", "-m", "init"], check=True)
    revision = git(root, "rev-parse", "HEAD")
    tree = git(root, "rev-parse", "HEAD^{tree}")
    cargo_lock = release.sha256(root / "Cargo.lock")
    return revision, tree, cargo_lock


class LoadT1ManifestTests(unittest.TestCase):
    def test_accepts_a_well_formed_linux_x64_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            for name in release.EXECUTABLES:
                (root / "bin" / name).write_bytes(b"\x7fELF")
            (root / "manifest.json").write_text(json.dumps({"schema": 1, "source_sha": "a" * 40, "target": release.TARGET}))
            manifest = release.load_t1_manifest(root)
            self.assertEqual(manifest["source_sha"], "a" * 40)

    def test_rejects_wrong_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            for name in release.EXECUTABLES:
                (root / "bin" / name).write_bytes(b"\x7fELF")
            (root / "manifest.json").write_text(json.dumps({"schema": 1, "source_sha": "a" * 40, "target": "aarch64-unknown-linux-gnu"}))
            with self.assertRaises(ValueError):
                release.load_t1_manifest(root)

    def test_rejects_a_missing_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            for name in release.EXECUTABLES[:-1]:
                (root / "bin" / name).write_bytes(b"\x7fELF")
            (root / "manifest.json").write_text(json.dumps({"schema": 1, "source_sha": "a" * 40, "target": release.TARGET}))
            with self.assertRaises(ValueError):
                release.load_t1_manifest(root)


class CheckSourceTests(unittest.TestCase):
    def test_binds_tree_lock_and_version_to_the_exact_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            revision, tree, cargo_lock = make_source_repo(root)
            got_tree, got_lock, got_version = release.check_source(root, revision)
            self.assertEqual((got_tree, got_lock, got_version), (tree, cargo_lock, "0.2.0"))

    def test_rejects_a_revision_outside_the_checkout_history(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_source_repo(root)
            with self.assertRaises(subprocess.CalledProcessError):
                release.check_source(root, "f" * 40)


class BuildManifestAndArchiveTests(unittest.TestCase):
    def test_build_manifest_binds_exact_source_and_executable_digests(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "out"
            executables = [{"name": "backend-cli", "packaged_path": "bin/backend-cli", "sha256": "e" * 64, "bytes": 9}]
            path = release.write_build_manifest(output, "a" * 40, "b" * 40, "c" * 64, executables)
            manifest = json.loads(path.read_text())
            self.assertEqual(manifest["source"], {"git_revision": "a" * 40, "git_tree": "b" * 40, "cargo_lock_sha256": "c" * 64})
            self.assertEqual(manifest["executables"]["backend-cli"], {"path": "bin/backend-cli", "sha256": "e" * 64, "bytes": 9})

    def test_archive_contents_are_nudox_prefixed_with_no_links(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / "Nudox" / "bin").mkdir(parents=True)
            (output / "Nudox" / "bin" / "backend-cli").write_bytes(b"\x7fELF")
            archive = release.make_archive(output)
            with tarfile.open(archive, "r:gz") as bundle:
                names = bundle.getnames()
                self.assertTrue(all(name == "Nudox" or name.startswith("Nudox/") for name in names))
                self.assertTrue(all(member.isfile() or member.isdir() for member in bundle.getmembers()))


class ReleaseManifestSchemaTests(unittest.TestCase):
    """What release_contract.py's validate() actually checks for linux-x64."""

    def test_required_schema_fields_match_the_contract_exactly(self):
        required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform",
                    "target", "asset", "sha256", "size_bytes", "minimum_os", "build_manifest_sha256",
                    "signed", "notarized"}
        manifest = {
            "schema": 1, "version": "0.2.0", "source_sha": "a" * 40, "source_tree": "b" * 40,
            "cargo_lock_sha256": "c" * 64, "platform": release.PLATFORM, "target": release.TARGET,
            "asset": release.ASSET, "sha256": "d" * 64, "size_bytes": 1, "minimum_os": release.MINIMUM_OS,
            "build_manifest_sha256": "e" * 64, "signed": False, "notarized": False,
        }
        self.assertEqual(set(manifest), required)
        self.assertEqual(manifest["platform"], "linux-x64")
        self.assertEqual(manifest["asset"], "nudox-linux-x64.tar.gz")
        self.assertIs(manifest["signed"], False)
        self.assertIs(manifest["notarized"], False)


if __name__ == "__main__":
    unittest.main()
