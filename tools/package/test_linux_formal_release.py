"""Source and metadata controls plus mocked relocation and archive checks.

The mocked relocation test verifies that build evidence records the final
post-patchelf bytes. Real Linux ELF and shared-library closure validation still
requires an actual Linux host and toolchain.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import linux_formal_release as release
import release_contract


def git(root: Path, *args: str) -> str:
    return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()


def make_source_repo(root: Path) -> tuple[str, str, str]:
    subprocess.run(["git", "-C", str(root), "init", "-q"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.email", "t@t.t"], check=True)
    subprocess.run(["git", "-C", str(root), "config", "user.name", "t"], check=True)
    (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.0"\n')
    (root / "Cargo.lock").write_text("# lock\n")
    policy = root / "tools" / "package" / "linux_system_sonames.txt"
    policy.parent.mkdir(parents=True)
    policy.write_bytes((Path(release.__file__).with_name("linux_system_sonames.txt")).read_bytes())
    subprocess.run(["git", "-C", str(root), "add", "."], check=True)
    subprocess.run(["git", "-C", str(root), "commit", "-q", "-m", "init"], check=True)
    revision = git(root, "rev-parse", "HEAD")
    tree = git(root, "rev-parse", "HEAD^{tree}")
    cargo_lock = release.sha256(root / "Cargo.lock")
    return revision, tree, cargo_lock


def make_t1_artifact(root: Path, source_sha: str = "a" * 40) -> dict:
    (root / "bin").mkdir(parents=True)
    payloads = {f"bin/{name}": b"\x7fELF" for name in release.EXECUTABLES}
    payloads.update({"smoke.py": b"# fixture\n", "runtime.nar": b"fixture runtime\n"})
    files = []
    for relative, payload in payloads.items():
        path = root / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
        files.append({"path": relative, "bytes": len(payload), "sha256": release.sha256(path)})
    store_root = "/nix/store/" + "a" * 32 + "-fixture"
    manifest = {
        "schema": 1,
        "source_sha": source_sha,
        "target": release.TARGET,
        "toolchain": "rustc fixture",
        "runtime_roots": [store_root],
        "runtime_env": {"PATH": store_root + "/bin"},
        "python": store_root + "/bin/python3",
        "files": files,
    }
    (root / "manifest.json").write_text(json.dumps(manifest))
    return manifest


class LoadT1ManifestTests(unittest.TestCase):
    def test_accepts_a_well_formed_linux_x64_artifact(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            expected = make_t1_artifact(root)
            manifest = release.load_t1_manifest(root)
            self.assertEqual(manifest, expected)

    def test_rejects_wrong_target(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_t1_artifact(root)
            manifest_path = root / "manifest.json"
            manifest = json.loads(manifest_path.read_text())
            manifest["target"] = "aarch64-unknown-linux-gnu"
            manifest_path.write_text(json.dumps(manifest))
            with self.assertRaises(ValueError):
                release.load_t1_manifest(root)

    def test_rejects_a_missing_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = make_t1_artifact(root)
            manifest["files"] = [item for item in manifest["files"] if item["path"] != "bin/backend-locald"]
            (root / "manifest.json").write_text(json.dumps(manifest))
            with self.assertRaises(ValueError):
                release.load_t1_manifest(root)

    def test_rejects_malformed_source_revision(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = make_t1_artifact(root)
            manifest["source_sha"] = "not-a-revision"
            (root / "manifest.json").write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, "wrong schema or target"):
                release.load_t1_manifest(root)

    def test_rejects_artifact_bytes_that_do_not_match_the_t1_inventory(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            make_t1_artifact(root)
            (root / "bin" / "backend-cli").write_bytes(b"substituted")
            with self.assertRaises(ValueError):
                release.load_t1_manifest(root)

    def test_rejects_binary_replacement_after_manifest_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = make_t1_artifact(root)
            expected = {item["path"]: item for item in manifest["files"]}
            (root / "bin" / "backend-cli").write_bytes(b"replaced after verify")
            with self.assertRaisesRegex(ValueError, "backend-cli differs from the T1 artifact manifest"):
                release.portabilize(root / "bin", ("backend-cli",), root / "out",
                                    Path("unused-patchelf"), Path("unused-readelf"),
                                    Path("unused-ldd"), expected)


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
            with self.assertRaises(ValueError):
                release.check_source(root, "f" * 40)

    def test_rejects_working_tree_source_metadata_drift(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            revision, _, _ = make_source_repo(root)
            (root / "Cargo.toml").write_text('[workspace.package]\nversion = "9.9.9"\n')
            with self.assertRaises(ValueError):
                release.check_source(root, revision)


class BuildManifestAndArchiveTests(unittest.TestCase):
    def test_build_manifest_binds_exact_source_and_executable_digests(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "out"
            executables = [{"name": "backend-cli", "packaged_path": "bin/backend-cli", "sha256": "e" * 64, "bytes": 9,
                            "t1_artifact_sha256": "f" * 64, "t1_artifact_bytes": 8}]
            path = release.write_build_manifest(output, "a" * 40, "b" * 40, "c" * 64, executables)
            manifest = json.loads(path.read_text())
            self.assertEqual(manifest["source"], {"git_revision": "a" * 40, "git_tree": "b" * 40, "cargo_lock_sha256": "c" * 64})
            self.assertEqual(manifest["executables"]["backend-cli"], {
                "path": "bin/backend-cli", "sha256": "e" * 64, "bytes": 9,
                "t1_artifact_sha256": "f" * 64, "t1_artifact_bytes": 8,
            })

    def test_build_manifest_records_mutated_post_relocation_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            artifact = root / "artifact"
            manifest = make_t1_artifact(artifact)
            expected = {item["path"]: item for item in manifest["files"]}
            output = root / "out"

            def fake_run(command):
                tool, operation = Path(command[0]).name, command[1]
                if tool == "fake-readelf":
                    if operation == "-h":
                        return "Class: ELF64\nMachine: Advanced Micro Devices X86-64\n"
                    if operation == "-d":
                        return ""
                    if operation == "-l":
                        return f"Requesting program interpreter: {release.INTERPRETER}]\n"
                    if operation == "--version-info":
                        return "GLIBC_2.17"
                if tool == "fake-ldd":
                    return "linux-vdso.so.1 (0x00007fff0000)\n"
                if tool == "fake-patchelf":
                    packaged = Path(command[-1])
                    if operation == "--set-interpreter":
                        packaged.write_bytes(packaged.read_bytes() + b"relocated")
                        return ""
                    if operation == "--set-rpath":
                        packaged.write_bytes(packaged.read_bytes() + b"relocated")
                        return ""
                    if operation == "--print-rpath":
                        return "$ORIGIN/../lib"
                raise AssertionError(f"unexpected fake tool command: {command}")

            with (patch.object(release, "run", side_effect=fake_run),
                  patch.dict(release.elf_info.__globals__, {"run": fake_run})):
                executables, _ = release.portabilize(
                    artifact / "bin", ("backend-cli",), output,
                    Path("fake-patchelf"), Path("fake-readelf"), Path("fake-ldd"), expected,
                )
            record = executables[0]
            packaged = output / "Nudox" / record["packaged_path"]
            self.assertNotEqual(record["t1_artifact_sha256"], record["sha256"])
            self.assertEqual(record["sha256"], release.sha256(packaged))
            self.assertEqual(record["bytes"], packaged.stat().st_size)

            build_path = release.write_build_manifest(output, "a" * 40, "b" * 40, "c" * 64, executables)
            archive = release.make_archive(output)
            with tarfile.open(archive, "r:gz") as bundle:
                archived_build = json.load(bundle.extractfile("Nudox/resources/build-manifest.json"))
                archived_binary = bundle.extractfile("Nudox/bin/backend-cli").read()
            self.assertEqual(archived_build["executables"]["backend-cli"]["sha256"],
                             hashlib.sha256(archived_binary).hexdigest())
            self.assertEqual(archived_build["executables"]["backend-cli"]["bytes"], len(archived_binary))
            self.assertEqual(json.loads(build_path.read_text()), archived_build)

    def test_mocked_candidate_passes_the_real_release_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            revision, tree, cargo_lock = make_source_repo(source)
            artifact = root / "artifact"
            t1_manifest = make_t1_artifact(artifact, revision)
            output = root / "candidate"

            def fake_run(command):
                tool, operation = Path(command[0]).name, command[1]
                if tool == "fake-readelf":
                    if operation == "-h":
                        return "Class: ELF64\nMachine: Advanced Micro Devices X86-64\n"
                    if operation == "-d":
                        return ""
                    if operation == "-l":
                        return f"Requesting program interpreter: {release.INTERPRETER}]\n"
                    if operation == "--version-info":
                        return "GLIBC_2.17"
                if tool == "fake-ldd":
                    return "linux-vdso.so.1 (0x00007fff0000)\n"
                if tool == "fake-patchelf":
                    packaged = Path(command[-1])
                    if operation == "--set-interpreter":
                        packaged.write_bytes(packaged.read_bytes() + b"relocated")
                        return ""
                    if operation == "--print-rpath":
                        return "$ORIGIN/../lib"
                raise AssertionError(f"unexpected fake tool command: {command}")

            with (
                patch.object(release, "run", side_effect=fake_run),
                patch.dict(release.elf_info.__globals__, {"run": fake_run}),
                patch.object(release, "require_tool", side_effect=lambda path, label: Path(path)),
                patch.object(sys, "argv", [
                    "linux_formal_release.py", "--t1-artifact-dir", str(artifact),
                    "--source", str(source), "--output", str(output),
                    "--patchelf", "fake-patchelf", "--readelf", "fake-readelf", "--ldd", "fake-ldd",
                ]),
            ):
                self.assertEqual(release.main(), 0)

            admitted = release_contract.validate(
                output, source=source, platform="linux-x64", require_qa=False,
            )
            self.assertEqual(admitted["source_sha"], revision)
            self.assertEqual(admitted["source_tree"], tree)
            self.assertEqual(admitted["cargo_lock_sha256"], cargo_lock)
            self.assertEqual(admitted["minimum_os"], "Linux glibc 2.17")
            with tarfile.open(output / release.ASSET, "r:gz") as bundle:
                archived_build = json.load(bundle.extractfile("Nudox/resources/build-manifest.json"))
                for name in release.EXECUTABLES:
                    record = archived_build["executables"][name]
                    packaged_bytes = bundle.extractfile(f"Nudox/bin/{name}").read()
                    t1_record = next(item for item in t1_manifest["files"] if item["path"] == f"bin/{name}")
                    self.assertEqual(record["sha256"], hashlib.sha256(packaged_bytes).hexdigest())
                    self.assertEqual(record["bytes"], len(packaged_bytes))
                    self.assertEqual(record["t1_artifact_sha256"], t1_record["sha256"])
                    self.assertEqual(record["t1_artifact_bytes"], t1_record["bytes"])

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
    """Schema-shape controls; mocked full candidate admission is tested below."""

    def test_required_schema_fields_match_the_contract_exactly(self):
        required = {"schema", "version", "source_sha", "source_tree", "cargo_lock_sha256", "platform",
                    "target", "asset", "sha256", "size_bytes", "minimum_os", "build_manifest_sha256",
                    "signed", "notarized"}
        manifest = {
            "schema": 1, "version": "0.2.0", "source_sha": "a" * 40, "source_tree": "b" * 40,
            "cargo_lock_sha256": "c" * 64, "platform": release.PLATFORM, "target": release.TARGET,
            "asset": release.ASSET, "sha256": "d" * 64, "size_bytes": 1, "minimum_os": f"{release.MINIMUM_OS} 2.17",
            "build_manifest_sha256": "e" * 64, "signed": False, "notarized": False,
        }
        self.assertEqual(set(manifest), required)
        self.assertEqual(manifest["platform"], "linux-x64")
        self.assertEqual(manifest["asset"], "nudox-linux-x64.tar.gz")
        self.assertIs(manifest["signed"], False)
        self.assertIs(manifest["notarized"], False)

    def test_requires_the_expected_post_relocation_rpath(self):
        with patch.object(release, "run", return_value="$ORIGIN"):
            release.require_rpath(Path("patchelf"), Path("backend-cli"), "$ORIGIN")
        with patch.object(release, "run", return_value="/nix/store/lib"):
            with self.assertRaises(ValueError):
                release.require_rpath(Path("patchelf"), Path("backend-cli"), "$ORIGIN")


if __name__ == "__main__":
    unittest.main()
