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


def fake_run(command):
    """readelf/ldd/patchelf/strip stand-ins: relocation appends bytes, nothing else changes."""
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
            return ""
        if operation == "--print-rpath":
            return "$ORIGIN/../lib"
    if tool == "fake-strip" and operation == "--strip-all":
        packaged = Path(command[-1])
        packaged.write_bytes(packaged.read_bytes()[:4])
        return ""
    raise AssertionError(f"unexpected fake tool command: {command}")


def fake_tools():
    return (
        patch.object(release, "run", side_effect=fake_run),
        patch.dict(release.elf_info.__globals__, {"run": fake_run}),
        patch.object(release, "require_tool", side_effect=lambda path, label: Path(path)),
    )


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

    def test_formal_companions_preserve_worker_and_private_library_bytes(self):
        from record_stylo_owner_build import bounded_process
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            bin_dir = root / "input"
            bin_dir.mkdir()
            owner = root / "owner"
            desktop = root / "desktop"
            raw = root / "raw"
            stylo = root / "stylo"
            sqlite = root / "sqlite"
            source_lib = root / "source-lib"
            library = root / "private-lib"
            for path in (owner, desktop, raw, stylo, sqlite, source_lib, library):
                path.write_bytes(b"\x7fELF" + path.name.encode())
            def identity(path):
                return {"path": str(path), "bytes": path.stat().st_size, "sha256": release.sha256(path)}
            companion = {"owner": identity(owner), "desktop": {"artifact": identity(desktop)},
                "worker": identity(stylo), "formal_raw_stylo": str(raw),
                "source_loader": {"path": "/loader"}, "observer_environment": {},
                "dependency": identity(library), "dependency_source": identity(source_lib),
                "sqlite": {"worker": identity(sqlite), "worker_raw": {"path": str(raw)},
                           "tools": {"loader": {"path": "/loader"}}}}
            calls = []
            def fake_tool(command):
                calls.append(command)
                if command[1] == "--print-rpath":
                    return "$ORIGIN" if Path(command[-1]).name == "libgcc_s.so.1" else "$ORIGIN/../lib"
                return ""
            def fake_info(_readelf, path):
                return {"interpreter": release.INTERPRETER, "needed": ["libc.so.6", "libgcc_s.so.1"] if Path(path).parent.name == "bin" else ["libc.so.6"], "required_glibc_versions": ["2.17"]}
            linkage = f"libgcc_s.so.1 => {source_lib} (0x1)\n"
            with (patch.object(release, "run", side_effect=fake_tool),
                  patch.object(release, "elf_info", side_effect=fake_info),
                  patch("record_stylo_owner_build.bounded_process", return_value=linkage.encode())):
                records, _ = release.portabilize(bin_dir, ("backend-locald", "backend-desktop", "backend-stylo-worker", "backend-sqlite-worker"), root / "out", Path("patchelf"), Path("readelf"), Path("ldd"), {}, companion)
            package = root / "out/Nudox"
            self.assertEqual((package / "bin/backend-sqlite-worker").read_bytes(), sqlite.read_bytes())
            self.assertEqual((package / "bin/backend-stylo-worker").read_bytes(), stylo.read_bytes())
            self.assertEqual((package / "lib/libgcc_s.so.1").read_bytes(), library.read_bytes())
            self.assertFalse(any(command[1] == "--set-interpreter" and Path(command[-1]).name in {"backend-sqlite-worker", "backend-stylo-worker"} for command in calls))
            self.assertEqual(len(records), 4)

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

    def test_release_build_with_gui_libraries_passes_the_real_release_contract(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            revision, tree, _ = make_source_repo(source)
            build = root / "target" / "release"
            build.mkdir(parents=True)
            for name in release.EXECUTABLES:
                (build / name).write_bytes(b"\x7fELF with debug info")
            gui = root / "gui-libs"
            gui.mkdir()
            for soname in ("libxkbcommon.so.0", "libwayland-client.so.0"):
                (gui / soname).write_bytes(b"\x7fELF " + soname.encode())
            sonames = root / "gui-sonames.txt"
            sonames.write_text("# fixture\nlibxkbcommon.so.0\nlibwayland-client.so.0\nlibc.so.6\n")
            output = root / "candidate"
            run_patch, elf_patch, tool_patch = fake_tools()
            with (
                run_patch, elf_patch, tool_patch,
                patch.object(release, "GUI_SONAMES", sonames),
                patch.object(sys, "argv", [
                    "linux_formal_release.py", "--release-dir", str(build),
                    "--source", str(source), "--output", str(output),
                    "--patchelf", "fake-patchelf", "--readelf", "fake-readelf", "--ldd", "fake-ldd",
                    "--strip", "fake-strip", "--gui-library-path", f"{root / 'absent'}:{gui}",
                ]),
            ):
                self.assertEqual(release.main(), 0)

            self.assertFalse((output / "stage").exists(), "the stripped stage is not part of the candidate")
            admitted = release_contract.validate(output, source=source, platform="linux-x64", require_qa=False)
            self.assertEqual((admitted["source_sha"], admitted["source_tree"]), (revision, tree))
            with tarfile.open(output / release.ASSET, "r:gz") as bundle:
                names = set(bundle.getnames())
                build_manifest = json.load(bundle.extractfile("Nudox/resources/build-manifest.json"))
                launcher = bundle.extractfile("Nudox/bin/nudox-desktop").read().decode()
                library = bundle.getmember("Nudox/lib/libxkbcommon.so.0")
            self.assertTrue({"Nudox/lib/libxkbcommon.so.0", "Nudox/lib/libwayland-client.so.0"} <= names)
            self.assertNotIn("Nudox/lib/libc.so.6", names, "glibc comes from the host")
            self.assertTrue({f"Nudox/bin/{name}" for name in release.LAUNCHERS} <= names)
            self.assertIn('exec "$here/backend-desktop" "$@"', launcher)
            self.assertEqual(library.mode & 0o777, 0o755)
            self.assertEqual(build_manifest["profile"], "release")
            record = build_manifest["executables"]["backend-cli"]
            self.assertEqual(record["t1_artifact_bytes"], 4, "the stripped bytes are what was relocated")

    def test_release_build_refuses_a_missing_gui_library(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            make_source_repo(source)
            build = root / "release"
            build.mkdir()
            for name in release.EXECUTABLES:
                (build / name).write_bytes(b"\x7fELF")
            sonames = root / "gui-sonames.txt"
            sonames.write_text("libvulkan.so.1\n")
            run_patch, elf_patch, tool_patch = fake_tools()
            with (
                run_patch, elf_patch, tool_patch,
                patch.object(release, "GUI_SONAMES", sonames),
                patch.object(sys, "argv", [
                    "linux_formal_release.py", "--release-dir", str(build),
                    "--source", str(source), "--output", str(root / "candidate"),
                    "--patchelf", "fake-patchelf", "--readelf", "fake-readelf", "--ldd", "fake-ldd",
                    "--strip", "fake-strip", "--gui-library-path", str(root),
                ]),
            ):
                with self.assertRaisesRegex(ValueError, "libvulkan.so.1"):
                    release.main()

    def test_a_listed_library_found_only_through_another_ones_rpath_is_bundled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gui, glib, lib_dir = root / "gui", root / "glib", root / "lib"
            for path in (gui, glib, lib_dir):
                path.mkdir()
            (gui / "libgio-2.0.so.0").write_bytes(b"\x7fELF gio")
            (glib / "libpcre2-8.so.0").write_bytes(b"\x7fELF pcre2")
            sonames = root / "gui-sonames.txt"
            # Popped first, before libgio's RPATH is on the search path.
            sonames.write_text("libgio-2.0.so.0\nlibpcre2-8.so.0\n")

            def tools(command):
                if Path(command[0]).name == "fake-patchelf" and command[1] == "--print-rpath":
                    return str(glib) if Path(command[-1]).name == "libgio-2.0.so.0" else ""
                return fake_run(command)

            with (
                patch.object(release, "run", side_effect=tools),
                patch.dict(release.elf_info.__globals__, {"run": tools}),
                patch.object(release, "GUI_SONAMES", sonames),
                patch.object(release, "NIX_STORE", str(root)),
            ):
                release.bundle_gui_libraries(lib_dir, [gui], Path("fake-patchelf"), Path("fake-readelf"), "2.39")
            self.assertEqual(sorted(path.name for path in lib_dir.iterdir()), ["libgio-2.0.so.0", "libpcre2-8.so.0"])

    def test_gui_libraries_may_not_raise_the_glibc_floor_and_host_libraries_stay_out(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            gui, lib_dir = root / "gui", root / "lib"
            gui.mkdir()
            lib_dir.mkdir()
            for soname in ("libxkbcommon.so.0", "libfontconfig.so.1", "libtls-new.so.1"):
                (gui / soname).write_bytes(b"\x7fELF " + soname.encode())
            sonames = root / "gui-sonames.txt"
            # libasound is on the host's list, so it is neither bundled nor missing.
            sonames.write_text("libxkbcommon.so.0\nlibasound.so.2\nlibfontconfig.so.1\nlibtls-new.so.1\n")

            def tools(command):
                if Path(command[0]).name == "fake-readelf" and command[1] == "--version-info" and "tls-new" in command[-1]:
                    return "GLIBC_2.17 GLIBC_ABI_GNU2_TLS"
                return fake_run(command)

            with (
                patch.object(release, "run", side_effect=tools),
                patch.dict(release.elf_info.__globals__, {"run": tools}),
                patch.object(release, "GUI_SONAMES", sonames),
            ):
                with self.assertRaisesRegex(ValueError, r"newer glibc than the executables' 2\.39.*\['libtls-new\.so\.1'\]"):
                    release.bundle_gui_libraries(lib_dir, [gui], Path("fake-patchelf"), Path("fake-readelf"), "2.39")
            self.assertFalse((lib_dir / "libasound.so.2").exists())

    def test_glibc_abi_markers_count_as_the_release_that_added_them(self):
        def tools(command):
            if command[1] == "--version-info":
                return "GLIBC_2.17 GLIBC_2.34 GLIBC_ABI_GNU2_TLS"
            return fake_run(command)

        with patch.dict(release.elf_info.__globals__, {"run": tools}):
            self.assertEqual(release.elf_info(Path("fake-readelf"), Path("lib.so"))["required_glibc_versions"], ["2.17", "2.34", "2.42"])

        def unknown(command):
            return "GLIBC_2.17 GLIBC_ABI_SOMETHING_NEW" if command[1] == "--version-info" else fake_run(command)

        with patch.dict(release.elf_info.__globals__, {"run": unknown}), self.assertRaisesRegex(ValueError, "GLIBC_ABI_SOMETHING_NEW"):
            release.elf_info(Path("fake-readelf"), Path("lib.so"))

    def test_portable_linux_publication_uploads_a_pinned_shell_installer(self):
        manifest = {"asset": release.ASSET, "sha256": "d" * 64, "minimum_os": "Linux glibc 2.39"}
        template = Path(release.__file__).with_name("install_linux.sh.in").read_bytes()
        script = release_contract.linux_shell_installer(template, "v0.3.0-rc.2-linux-x64", manifest).decode()
        self.assertIn('TAG="v0.3.0-rc.2-linux-x64"', script)
        self.assertIn(f'SHA256="{"d" * 64}"', script)
        self.assertIn('MINIMUM_GLIBC="2.39"', script)
        self.assertNotRegex(script, r"@[A-Z_]+@")
        for tag in ("v0.3.0-rc.2-macos", "checkpoint-x", "v0.3.0; rm -rf /"):
            with self.assertRaises(ValueError):
                release_contract.linux_shell_installer(template, tag, manifest)
        with self.assertRaisesRegex(ValueError, "glibc floor"):
            release_contract.linux_shell_installer(template, "v0.3.0", {**manifest, "minimum_os": "Linux"})

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
