"""Packaging regressions; synthetic ELF/tool fixtures never execute product images."""
import json
from pathlib import Path
import tempfile
import unittest
import os
import hashlib
import subprocess
import io
import tarfile
from contextlib import redirect_stdout
from unittest.mock import patch

import linux_release_package as package


class PublicBuildManifestTests(unittest.TestCase):
    def test_public_manifest_retains_digests_but_removes_builder_paths(self):
        build = {
            "schema": "nudox.runtime-build-manifest.v1",
            "source": {"commit": "a" * 40, "tree": "b" * 40, "clean": True},
            "executables": {
                name: {"path": f"/nix/store/hash/build/{name}", "sha256": "c" * 64, "bytes": 1234}
                for name in package.BINARIES
            },
            "root_receipt": {
                "schema": "nudox.runtime-artifact-build-receipt.v1",
                "path": "/root/private-receipts/cargo.json",
                "sha256": "d" * 64,
            },
        }

        public = package.public_build_manifest(build)
        encoded = json.dumps(public)

        self.assertEqual(public["source"], build["source"])
        self.assertEqual(public["root_receipt"], {"schema": build["root_receipt"]["schema"], "sha256": "d" * 64})
        self.assertTrue(all("path" not in record for record in public["executables"].values()))
        self.assertNotIn("/nix/store", encoded)
        self.assertNotIn("/root/private-receipts", encoded)
        self.assertEqual(public["executables"]["backend-mcp"]["sha256"], "c" * 64)

    def test_public_manifest_rejects_missing_receipt_identity(self):
        with self.assertRaisesRegex(ValueError, "hash-bound runtime artifact receipt"):
            package.public_build_manifest({
                "schema": "nudox.runtime-build-manifest.v1",
                "source": {"commit": "a" * 40, "tree": "b" * 40, "clean": True},
                "executables": {
                    name: {"path": f"/build/{name}", "sha256": "c" * 64, "bytes": 1234}
                    for name in package.BINARIES
                },
                "root_receipt": {"schema": "unexpected", "path": "/build/receipt", "sha256": "d" * 64},
            })


class RootArtifactReceiptTests(unittest.TestCase):
    def fixture(self, root):
        source = {"commit": "a" * 40, "tree": "b" * 40, "clean": True}
        artifacts = {name: {"sha256": "c" * 64, "bytes": 1234} for name in package.BINARIES}
        receipt = {"schema": "nudox.runtime-artifact-build-receipt.v1", "exit": 0,
                   "source": {**source, "clean_before": True, "clean_after": True},
                   "toolchain": {"unchanged": True}, "artifacts": artifacts}
        path = root / "receipt.json"
        build = {"schema": "nudox.runtime-build-manifest.v1", "source": source,
                 "executables": {name: {**record, "path": str(root / name)} for name, record in artifacts.items()},
                 "root_receipt": {"schema": receipt["schema"], "path": str(path)}}
        self.seal(path, receipt, build)
        return path, receipt, build

    def seal(self, path, receipt, build):
        path.write_text(json.dumps(receipt))
        build["root_receipt"]["sha256"] = package.sha256(path)

    def test_root_receipt_binds_exact_schema_and_all_three_artifact_digests_and_lengths(self):
        with tempfile.TemporaryDirectory() as directory:
            path, receipt, build = self.fixture(Path(directory).resolve())
            self.assertEqual(package.admit_receipt(build), receipt)
            for field, value in (("sha256", "d" * 64), ("bytes", 1235)):
                with self.subTest(field=field):
                    previous = receipt["artifacts"]["backend-cli"][field]
                    receipt["artifacts"]["backend-cli"][field] = value
                    self.seal(path, receipt, build)
                    with self.assertRaisesRegex(ValueError, "artifact differs"):
                        package.admit_receipt(build)
                    receipt["artifacts"]["backend-cli"][field] = previous
            receipt["schema"] = "unexpected"
            self.seal(path, receipt, build)
            with self.assertRaisesRegex(ValueError, "mismatched schema"):
                package.admit_receipt(build)
            receipt["schema"] = build["root_receipt"]["schema"]
            receipt["artifacts"].pop("backend-mcp")
            self.seal(path, receipt, build)
            with self.assertRaisesRegex(ValueError, "exactly the three"):
                package.admit_receipt(build)

    def test_root_receipt_rejects_float_and_boolean_integer_aliases(self):
        with tempfile.TemporaryDirectory() as directory:
            path, receipt, build = self.fixture(Path(directory).resolve())
            receipt["artifacts"]["backend-cli"]["bytes"] = 1234.0
            self.seal(path, receipt, build)
            with self.assertRaisesRegex(ValueError, "invalid artifact byte count"):
                package.admit_receipt(build)
            receipt["artifacts"]["backend-cli"]["bytes"] = 1234
            for value in (False, 0.0):
                with self.subTest(exit=value):
                    receipt["exit"] = value
                    self.seal(path, receipt, build)
                    with self.assertRaisesRegex(ValueError, "exact successful integer exit"):
                        package.admit_receipt(build)
            receipt["exit"] = 0
            build["executables"]["backend-cli"]["bytes"] = 1
            receipt["artifacts"]["backend-cli"]["bytes"] = True
            self.seal(path, receipt, build)
            with self.assertRaisesRegex(ValueError, "invalid artifact byte count"):
                package.admit_receipt(build)

    def test_initial_sparse_artifact_record_refuses_before_payload_hash_or_stage(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            path, receipt, build = self.fixture(root)
            artifact = root / "backend-cli"
            oversized = 8 * 1024 * 1024 * 1024
            with artifact.open("wb") as file:
                file.truncate(oversized)
            build["executables"]["backend-cli"]["bytes"] = oversized
            receipt["artifacts"]["backend-cli"]["bytes"] = oversized
            self.seal(path, receipt, build)
            with patch.object(package, "admit_file_digest") as hash_payload:
                with self.assertRaisesRegex(ValueError, "invalid backend-cli size"):
                    package.admit_receipt(build)
                hash_payload.assert_not_called()
            self.assertFalse((root / "stage").exists())


class SDKReceiptAdmissionTests(unittest.TestCase):
    """Filesystem/receipt controls only: no SDK or product executable is run."""
    def fixture(self, root):
        source = {"commit": "a" * 40, "tree": "b" * 40, "clean": True}
        files = {}
        for relative in ("node/bin/node", "node/LICENSE", "node_modules/typescript/bin/tsc",
                         "node_modules/typescript/lib/typescript.js", "node_modules/typescript/LICENSE.txt",
                         "node_modules/typescript/ThirdPartyNoticeText.txt"):
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(b"receipt fixture, never executed")
            files[relative] = package.sha256(path)
        (root / "node/bin/node").chmod(0o700)
        metadata = root / "node_modules/typescript/package.json"
        metadata.write_text(json.dumps({"name": "typescript", "version": "5.9.3"}))
        files["node_modules/typescript/package.json"] = package.sha256(metadata)
        receipt = {"schema": "nudox.typescript-sdk.v1", "source": source, "target": package.TARGET,
                   "files": files, "tools": {"node": {"version": "v24.18.0", "sha256": files["node/bin/node"]},
                                               "typescript": {"version": "5.9.3"}}}
        receipt_path = root.parent / "receipt.json"
        receipt_path.write_text(json.dumps(receipt))
        return receipt_path, receipt, source

    def test_complete_sdk_receipt_refuses_tree_and_origin_changes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "payload"
            root.mkdir()
            receipt_path, receipt, source = self.fixture(root)
            admitted, files, digest, sizes = package.admit_typescript_sdk(root, receipt_path, source)
            self.assertEqual(digest, package.sha256(receipt_path))
            self.assertEqual(admitted, receipt)
            self.assertEqual(set(files), set(receipt["files"]))
            with self.assertRaisesRegex(ValueError, "exact application source"):
                package.admit_typescript_sdk(root, receipt_path, {**source, "commit": "c" * 40})
            extra = root / "unreceipted-empty-directory"
            extra.mkdir()
            with self.assertRaisesRegex(ValueError, "unreceipted directory"):
                package.admit_typescript_sdk(root, receipt_path, source)
            extra.rmdir()
            api = root / "node_modules/typescript/lib/typescript.js"
            api.write_bytes(b"changed API")
            with self.assertRaisesRegex(ValueError, "differs from its receipt"):
                package.admit_typescript_sdk(root, receipt_path, source)

    def test_direct_sdk_receipt_cannot_exceed_runtime_typescript_package_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "payload"
            root.mkdir()
            receipt_path, receipt, source = self.fixture(root)
            api = root / "node_modules/typescript/lib/typescript.js"
            with api.open("r+b") as file:
                file.truncate(package.MAX_TYPESCRIPT_PACKAGE_BYTES + 1)
            # The claimed digest cannot authorize an over-bound package.
            with self.assertRaisesRegex(ValueError, "TypeScript package exceeds its 96 MiB"):
                package.admit_typescript_sdk(root, receipt_path, source)

    def test_sdk_receipt_cannot_add_an_escape_or_linked_member(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "payload"
            root.mkdir()
            receipt_path, receipt, source = self.fixture(root)
            receipt["files"]["../outside"] = "d" * 64
            receipt_path.write_text(json.dumps(receipt))
            with self.assertRaisesRegex(ValueError, "unsafe file path"):
                package.admit_typescript_sdk(root, receipt_path, source)
            del receipt["files"]["../outside"]
            receipt_path.write_text(json.dumps(receipt))
            member = root / "node_modules/typescript/lib/typescript.js"
            original = root.parent / "typescript-api.saved"
            member.rename(original)
            os.symlink(original, member)
            with self.assertRaisesRegex(ValueError, "link or special file"):
                package.admit_typescript_sdk(root, receipt_path, source)


class CopiedAdmissionTests(unittest.TestCase):
    def test_oversized_sparse_replacement_refuses_before_creating_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            target = Path(directory) / "stage"
            original = b"original admitted bytes"
            source.write_bytes(original)
            digest = hashlib.sha256(original).hexdigest()
            with source.open("r+b") as file:
                file.truncate(8 * 1024 * 1024 * 1024)
            with self.assertRaisesRegex(ValueError, "original admitted length"):
                package.copy_admitted_file(source, target, digest, len(original), 512 * 1024 * 1024)
            self.assertFalse(target.exists())

    def test_sdk_builder_refuses_output_alias_back_into_the_source_package(self):
        import build_typescript_sdk as builder
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory).resolve()
            payload = base / "payload"
            payload.mkdir()
            _, _, source = SDKReceiptAdmissionTests().fixture(payload)
            manifest = base / "build.json"
            manifest.write_text(json.dumps({"schema":"nudox.runtime-build-manifest.v1", "source":source}))
            package_root = payload / "node_modules/typescript"
            alias = base / "alias"
            os.symlink(package_root, alias)
            arguments = ["builder", "--manifest", str(manifest), "--node", str(payload / "node/bin/node"),
                         "--node-version", "v24.18.0", "--node-license", str(payload / "node/LICENSE"),
                         "--node-license-origin", "https://example.invalid/fixture-notice",
                         "--typescript-package", str(package_root), "--output", str(alias / "stage")]
            with patch.object(builder.sys, "argv", arguments):
                with self.assertRaisesRegex(ValueError, "outside the installed package"):
                    builder.main()
            self.assertFalse((package_root / "stage").exists())

    def test_sdk_change_after_admission_cannot_be_reinventoried_as_the_old_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "payload"
            root.mkdir()
            receipt_path, receipt, source = SDKReceiptAdmissionTests().fixture(root)
            _, paths, admitted_receipt_digest, sizes = package.admit_typescript_sdk(root, receipt_path, source)
            for relative in ("node_modules/typescript/lib/typescript.js", "node/bin/node"):
                member = paths[relative]
                member.write_bytes(b"x" * sizes[relative])
                target = root.parent / ("staged-" + member.name)
                with self.assertRaisesRegex(ValueError, "original admitted digest"):
                    package.copy_admitted_file(member, target, receipt["files"][relative], sizes[relative], 512 * 1024 * 1024)
                self.assertFalse(target.exists())
            receipt_path.write_bytes(b"changed receipt after parsed admission")
            self.assertNotEqual(admitted_receipt_digest, package.sha256(receipt_path))

    def test_build_and_library_original_digests_bind_pre_relocation_copy(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "source"
            target = Path(directory) / "stage"
            original = b"exact admitted artifact bytes; no ELF execution"
            source.write_bytes(original)
            digest = hashlib.sha256(original).hexdigest()
            package.copy_admitted_file(source, target, digest, len(original), len(original))
            self.assertEqual(target.read_bytes(), original)
            target.unlink()
            source.write_bytes(b"x" * len(original))
            with self.assertRaisesRegex(ValueError, "original admitted digest"):
                package.copy_admitted_file(source, target, digest, len(original), len(original))
            self.assertFalse(target.exists())

    def test_docs_descendant_keeps_exact_build_policy_but_policy_descendant_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.PIPE, text=True).strip()
            git("init", "-q")
            policy = root / "tools/package/linux_system_sonames.txt"
            policy.parent.mkdir(parents=True)
            policy.write_bytes(package.LINUX_SYSTEM_SONAMES_BYTES)
            git("add", ".")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "exact compiled policy")
            compiled = git("rev-parse", "HEAD")
            (root / "docs-only.txt").write_text("documentation descendant")
            git("add", ".")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "documentation only")
            package.check_loader_policy(root, compiled)
            # Compare the actual loaded packager policy with the older compiled file.
            import unittest.mock
            changed = package.LINUX_SYSTEM_SONAMES_BYTES.replace(b"libanl.so.1\n", b"")
            policy.write_bytes(changed)
            git("add", ".")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "loader policy descendant")
            with unittest.mock.patch.object(package, "LINUX_SYSTEM_SONAMES_BYTES", policy.read_bytes()):
                with self.assertRaisesRegex(ValueError, "exact compiled build source"):
                    package.check_loader_policy(root, compiled)


class NativeDefaultPackageTests(unittest.TestCase):
    """Exercise main/receipts/copies/archive with synthetic ELF tool responses only."""
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.source = self.base / "source"
        self.source.mkdir()
        def git(*args):
            return subprocess.check_output(["git", "-C", str(self.source), *args], stderr=subprocess.PIPE, text=True).strip()
        git("init", "-q")
        (self.source / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.0"\n')
        (self.source / "Cargo.lock").write_bytes(b"fixture lock\n")
        policy = self.source / "tools/package/linux_system_sonames.txt"
        policy.parent.mkdir(parents=True)
        policy.write_bytes(package.LINUX_SYSTEM_SONAMES_BYTES)
        git("add", ".")
        git("-c", "core.hooksPath=/dev/null", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "synthetic build source")
        source = {"commit": git("rev-parse", "HEAD"), "tree": git("rev-parse", "HEAD^{tree}"), "clean": True}
        receipt_path, receipt, self.build = RootArtifactReceiptTests().fixture(self.base)
        self.build["source"] = source
        receipt["source"] = {**source, "clean_before": True, "clean_after": True,
                             "cargo_lock_sha256": package.sha256(self.source / "Cargo.lock")}
        for name in package.BINARIES:
            image = self.base / name
            image.write_bytes(b"\x7fELF synthetic, never executed: " + name.encode())
            record = {"sha256": package.sha256(image), "bytes": image.stat().st_size}
            self.build["executables"][name].update(record)
            receipt["artifacts"][name] = record
        RootArtifactReceiptTests().seal(receipt_path, receipt, self.build)
        self.manifest = self.base / "build.json"
        self.manifest.write_text(json.dumps(self.build))
        self.output = self.base / "candidate"
        self.library = self.base / "libfixture.so"
        self.library.write_bytes(b"\x7fELF synthetic dependency, never executed")
        self.tools = {}
        for name in ("patchelf", "readelf", "ldd"):
            tool = self.base / name
            tool.write_bytes(b"synthetic tool, never executed")
            tool.chmod(0o700)
            self.tools[name] = str(tool)
        self.arguments = ["packager", "--manifest", str(self.manifest), "--source", str(self.source),
                          "--output", str(self.output)]
        for name, path in self.tools.items():
            self.arguments.extend(["--" + name, path])
        self.rpaths = {}
        self.node_probes = []
        self.node_versions = ("v24.18.0", "5.9.3")

    def sdk(self):
        root = self.base / "sdk-input/payload"
        root.mkdir(parents=True)
        receipt_path, receipt, _ = SDKReceiptAdmissionTests().fixture(root)
        receipt["source"] = self.build["source"]
        node = root / "node/bin/node"
        node.write_bytes(b"\x7fELF synthetic Node, never executed")
        receipt["files"]["node/bin/node"] = package.sha256(node)
        receipt["tools"]["node"]["sha256"] = package.sha256(node)
        receipt_path.write_text(json.dumps(receipt))
        return root, receipt_path

    def invoke(self, extra=()):
        # Only git is really executed. All ELF/tool and optional Node responses are
        # synthetic; admission, bounded copies, inventory and archive are production.
        real_run = package.run
        real_subprocess_run = subprocess.run
        def tool_run(command):
            tool, *args = command
            if tool == "git":
                return real_run(command)
            if tool == self.tools["ldd"]:
                return f"libfixture.so => {self.library} (0x1)\nlibc.so.6 => /synthetic/libc.so.6 (0x2)"
            if tool == self.tools["patchelf"]:
                if args == ["--version"]:
                    return "patchelf synthetic"
                if args[0] == "--print-rpath":
                    return self.rpaths[args[-1]]
                self.rpaths[args[-1]] = args[args.index("--set-rpath") + 1]
                return ""
            if tool == self.tools["readelf"]:
                if args == ["--version"]:
                    return "readelf synthetic"
                self.assertTrue(Path(args[-1]).read_bytes().startswith(b"\x7fELF"))
                if args[0] == "-h":
                    return "Class: ELF64\nMachine: Advanced Micro Devices X86-64"
                if args[0] == "-d":
                    return '(NEEDED) [libc.so.6]' + ('' if Path(args[-1]).name == 'libfixture.so' else '\n(NEEDED) [libfixture.so]')
                if args[0] == "-l":
                    return f"Requesting program interpreter: {package.INTERPRETER}]"
                self.assertEqual(args[0], "--version-info")
                return "GLIBC_2.31"
            self.fail(f"unexpected tool execution: {command}")
        def process_run(command, **kwargs):
            if command[0] == "git":
                return real_subprocess_run(command, **kwargs)
            expected_node = str(self.output / "nudox-linux-x86_64/share/nudox/typescript/node/bin/node")
            self.assertEqual(command[0], expected_node)
            self.assertEqual(kwargs["timeout"], 15)
            self.assertTrue({"LD_PRELOAD", "NODE_OPTIONS"}.isdisjoint(kwargs["env"]))
            self.node_probes.append(command[1:])
            version = self.node_versions[0 if command[1:] == ["--version"] else 1]
            return subprocess.CompletedProcess(command, 0, version, "")
        self.packaging_stdout = io.StringIO()
        with patch.object(package.sys, "argv", [*self.arguments, *extra]), \
             patch.object(package, "run", side_effect=tool_run), \
             patch.object(package.subprocess, "run", side_effect=process_run), \
             redirect_stdout(self.packaging_stdout):
            return package.main()

    def test_default_native_package_preserves_source_and_closure_without_sdk_or_probes(self):
        self.assertEqual(self.invoke(), 0)
        root = self.output / "nudox-linux-x86_64"
        manifest = json.loads((root / "packaging-manifest.json").read_text())
        self.assertEqual(manifest["typescript_sdk"], {"bundled": False})
        self.assertEqual(manifest["source"], self.build["source"])
        self.assertEqual({record["name"] for record in manifest["executables"]}, set(package.BINARIES))
        self.assertEqual(set(manifest["libraries"]), {"libfixture.so"})
        self.assertEqual(self.node_probes, [])
        self.assertFalse((root / "share/nudox/typescript").exists())
        readme = (root / "README.txt").read_text()
        self.assertIn("linked TSZ with embedded declarations", readme)
        self.assertIn("linked Pyrefly", readme)
        self.assertNotIn("archive also includes", readme)
        release = json.loads((self.output / "release-manifest.json").read_text())
        self.assertEqual(release["source_sha"], self.build["source"]["commit"])
        self.assertEqual(release["packaging_manifest_sha256"], package.sha256(root / "packaging-manifest.json"))
        self.assertEqual(manifest["checkpoint_shell_asset"], "install-linux-x64.sh")
        shell = self.output / "install-linux-x64.sh"
        self.assertEqual(shell.read_bytes(), package.checkpoint_shell(
            manifest["release_tag"], (self.output / "install-linux-x64.py").read_bytes(),
            release["minimum_glibc"]))
        self.assertEqual((self.output / "install-linux-x64.sh.sha256").read_text(),
                         f"{package.sha256(shell)}  install-linux-x64.sh\n")
        self.assertEqual(json.loads(self.packaging_stdout.getvalue())["shell_installer"],
                         {"path": str(shell), "sha256": package.sha256(shell),
                          "size_bytes": shell.stat().st_size})
        with tarfile.open(self.output / release["asset"]) as archive:
            self.assertFalse(any("/typescript/" in name for name in archive.getnames()))
        import install_linux as installer
        with patch.object(installer.subprocess, "run") as probe:
            installer.verify_typescript_sdk(root, manifest)
            probe.assert_not_called()

    def test_explicit_sdk_retains_inventory_relocation_and_both_version_probes(self):
        sdk, receipt = self.sdk()
        self.assertEqual(self.invoke(["--typescript-sdk-directory", str(sdk), "--typescript-sdk-receipt", str(receipt)]), 0)
        root = self.output / "nudox-linux-x86_64"
        manifest = json.loads((root / "packaging-manifest.json").read_text())
        bundled = manifest["typescript_sdk"]
        self.assertTrue(bundled["bundled"])
        self.assertEqual(bundled["source_receipt_sha256"], package.sha256(receipt))
        self.assertEqual(len(bundled["files"]), 7)
        self.assertEqual(len(self.node_probes), 2)
        self.assertEqual(self.node_probes[0], ["--version"])
        self.assertEqual(self.node_probes[1], ["-e", "process.stdout.write(require(process.argv[1]).version)", str(root / "share/nudox/typescript/node_modules/typescript/lib/typescript.js")])
        self.assertEqual(self.rpaths[str(root / bundled["node"]["packaged_path"])], "$ORIGIN/../../../../../lib")
        self.assertEqual({record["name"] for record in manifest["executables"]}, set(package.BINARIES))
        self.assertIn("explicitly\nconfigured legacy Compiler API", (root / "README.txt").read_text())

    def test_sdk_half_pair_refuses_before_manifest_read_or_stage(self):
        self.manifest.unlink()
        for flag in ("--typescript-sdk-directory", "--typescript-sdk-receipt"):
            with self.subTest(flag=flag), self.assertRaisesRegex(ValueError, "must be provided together"):
                self.invoke([flag, str(self.base / "missing")])
        self.assertFalse(self.output.exists())

    def test_optional_sdk_does_not_weaken_source_or_payload_hash_admission(self):
        sdk, receipt = self.sdk()
        api = sdk / "node_modules/typescript/lib/typescript.js"
        api.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "differs from its receipt"):
            self.invoke(["--typescript-sdk-directory", str(sdk), "--typescript-sdk-receipt", str(receipt)])
        self.assertFalse(self.output.exists())

    def test_explicit_sdk_wrong_source_and_relocated_api_mismatch_refuse(self):
        sdk, receipt_path = self.sdk()
        receipt = json.loads(receipt_path.read_text())
        receipt["source"]["commit"] = "0" * 40
        receipt_path.write_text(json.dumps(receipt))
        arguments = ["--typescript-sdk-directory", str(sdk), "--typescript-sdk-receipt", str(receipt_path)]
        with self.assertRaisesRegex(ValueError, "exact application source"):
            self.invoke(arguments)
        self.assertFalse(self.output.exists())
        receipt["source"] = self.build["source"]
        receipt_path.write_text(json.dumps(receipt))
        self.node_versions = ("v24.18.0", "wrong API")
        with self.assertRaisesRegex(ValueError, "relocated SDK TypeScript Compiler API"):
            self.invoke(arguments)
        self.assertFalse((self.output / "release-manifest.json").exists())

    def test_no_sdk_still_rejects_tampered_product_bytes(self):
        Path(self.build["executables"]["backend-mcp"]["path"]).write_bytes(b"changed product")
        with self.assertRaisesRegex(ValueError, "differs from the successful build receipt"):
            self.invoke()
        self.assertFalse((self.output / "release-manifest.json").exists())

    def test_help_describes_default_and_paired_optional_sdk(self):
        output = io.StringIO()
        with patch.object(package.sys, "argv", ["packager", "--help"]), redirect_stdout(output):
            with self.assertRaises(SystemExit) as stopped:
                package.main()
        self.assertEqual(stopped.exception.code, 0)
        self.assertIn("linked TSZ", output.getvalue())
        self.assertIn("[--typescript-sdk-directory", output.getvalue())
        self.assertIn("requires --typescript-sdk-receipt", " ".join(output.getvalue().split()))


class LinkageTests(unittest.TestCase):
    def test_actual_nix_loader_alias_is_excluded_from_dependency_closure(self):
        linkage = """linux-vdso.so.1 (0x00007fff)
 libgcc_s.so.1 => /nix/store/gcc/lib/libgcc_s.so.1 (0x00007fff)
 libm.so.6 => /nix/store/glibc/lib/libm.so.6 (0x00007fff)
 libc.so.6 => /nix/store/glibc/lib/libc.so.6 (0x00007fff)
/nix/store/old-glibc/lib/ld-linux-x86-64.so.2 => /nix/store/glibc/lib64/ld-linux-x86-64.so.2 (0x00007fff)
"""
        self.assertEqual(package.ldd_dependencies(linkage), [
            ("libgcc_s.so.1", "/nix/store/gcc/lib/libgcc_s.so.1"),
            ("libm.so.6", "/nix/store/glibc/lib/libm.so.6"),
            ("libc.so.6", "/nix/store/glibc/lib/libc.so.6"),
        ])

    def test_path_qualified_other_dependencies_are_rejected_before_copy(self):
        for name in ("/tmp/libgcc_s.so.1", "../outside", "..", "lib\\escape.so"):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "unsafe"):
                package.ldd_dependencies(f"{name} => /tmp/dependency.so (0x1)")

    def test_loader_basename_alone_cannot_authorize_a_different_target(self):
        with self.assertRaisesRegex(ValueError, "unsafe"):
            package.ldd_dependencies("/tmp/ld-linux-x86-64.so.2 => /tmp/other.so (0x1)")


if __name__ == "__main__":
    unittest.main()
