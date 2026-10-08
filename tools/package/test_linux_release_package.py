"""Packaging-evidence regressions; these tests never invoke or fake a product ELF."""
import json
from pathlib import Path
import tempfile
import unittest
import os
import hashlib
import subprocess

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
