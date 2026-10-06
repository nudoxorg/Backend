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


if __name__ == "__main__":
    unittest.main()
