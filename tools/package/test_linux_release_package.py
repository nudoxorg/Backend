"""Packaging-evidence regressions; these tests never invoke or fake a product ELF."""
import json
import unittest

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
