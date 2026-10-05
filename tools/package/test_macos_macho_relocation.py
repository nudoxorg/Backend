#!/usr/bin/env python3
"""Data-contract tests for admitted macOS runtime and Mach-O relocation inputs.

These tests use ordinary files and do not establish native Mach-O closure,
codesign, app-launch, or runtime behavior.
"""

from __future__ import annotations

import importlib.util
import json
import tarfile
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any


PACKAGE_DIR = Path(__file__).resolve().parent


def load_module(filename: str, name: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, PACKAGE_DIR / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


relocation = load_module("macho_relocation.py", "macho_relocation")


class DotnetRuntimeReceiptTests(unittest.TestCase):
    def _runtime(self, root: Path) -> Path:
        (root / "host/fxr/8.0.22").mkdir(parents=True)
        (root / "shared/Microsoft.NETCore.App/8.0.22").mkdir(parents=True)
        (root / "dotnet").write_bytes(b"runtime host")
        (root / "dotnet").chmod(0o755)
        (root / "host/fxr/8.0.22/libhostfxr.dylib").write_bytes(b"hostfxr")
        (root / "shared/Microsoft.NETCore.App/8.0.22/System.Private.CoreLib.dll").write_bytes(b"corelib")
        (root / "LICENSE.txt").write_text("license\n", encoding="utf-8")
        (root / "ThirdPartyNotices.txt").write_text("third party\n", encoding="utf-8")
        return root

    def _pin(self, root: Path, archive: Path) -> dict[str, Any]:
        manifest = relocation.file_tree_manifest(root)
        return {
            "schema": 1,
            "target": "aarch64-apple-darwin",
            "runtime_identifier": "osx-arm64",
            "package": {"name": "Microsoft.NETCore.App", "version": "8.0.22"},
            "distribution": {
                "kind": "verified-archive",
                "locator": "https://example.invalid/dotnet-runtime-osx-arm64.tar.gz",
                "archive_sha256": relocation._sha256(archive),
            },
            "root_tree_sha256": relocation.file_tree_sha256(manifest),
            "notices": {
                "license": {
                    "path": "LICENSE.txt",
                    "sha256": manifest["LICENSE.txt"]["sha256"],
                },
                "third_party": {
                    "path": "ThirdPartyNotices.txt",
                    "sha256": manifest["ThirdPartyNotices.txt"]["sha256"],
                },
            },
        }

    def _archive(self, root: Path, archive: Path) -> None:
        with tarfile.open(archive, "w:gz") as stream:
            stream.add(root, arcname=".")

    def test_receipt_binds_complete_runtime_root_archive_and_notices(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = self._runtime(Path(temporary) / "runtime")
            archive = Path(temporary) / "dotnet-runtime.tar.gz"
            self._archive(root, archive)
            pin_path = Path(temporary) / "runtime-pin.json"
            pin_path.write_text(json.dumps(self._pin(root, archive)), encoding="utf-8")
            receipt = relocation.create_dotnet_runtime_receipt(
                root, pin_path, "aarch64-apple-darwin", archive
            )
            observed = relocation.validate_dotnet_runtime_receipt(
                receipt, root, "aarch64-apple-darwin", pin_path, archive
            )
            self.assertEqual(receipt["kind"], "dotnet-runtime")
            self.assertEqual(receipt["package"]["version"], "8.0.22")
            self.assertEqual(observed["dotnet"]["mode"], 0o755)

    def test_receipt_rejects_changed_notice_or_unlisted_runtime_file(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = self._runtime(Path(temporary) / "runtime")
            archive = Path(temporary) / "dotnet-runtime.tar.gz"
            self._archive(root, archive)
            pin_path = Path(temporary) / "runtime-pin.json"
            pin_path.write_text(json.dumps(self._pin(root, archive)), encoding="utf-8")
            receipt = relocation.create_dotnet_runtime_receipt(
                root, pin_path, "aarch64-apple-darwin", archive
            )
            (root / "ThirdPartyNotices.txt").write_text("changed\n", encoding="utf-8")
            with self.assertRaises(relocation.RelocationInputError):
                relocation.validate_dotnet_runtime_receipt(
                    receipt, root, "aarch64-apple-darwin", pin_path, archive
                )

    def test_nix_runtime_pin_must_match_exact_store_output_path(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = self._runtime(Path(temporary) / "runtime")
            archive = Path(temporary) / "unused"
            self._archive(root, archive)
            pin = self._pin(root, archive)
            pin["distribution"] = {
                "kind": "pinned-nix-output",
                "locator": "dotnet-sdk-8.0.422-osx-arm64",
                "store_output": "0123456789abcdefghijklmnopqrstuv-dotnet-sdk-8.0.422",
                "derivation_sha256": "1" * 64,
            }
            pin_path = Path(temporary) / "runtime-pin.json"
            pin_path.write_text(json.dumps(pin), encoding="utf-8")
            with self.assertRaises(relocation.RelocationInputError):
                relocation.create_dotnet_runtime_receipt(
                    root, pin_path, "aarch64-apple-darwin"
                )

    def test_runtime_symlink_cannot_escape_admitted_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            parent = Path(temporary)
            root = self._runtime(parent / "runtime")
            (root / "escape.dylib").symlink_to(parent / "outside.dylib")
            (parent / "outside.dylib").write_bytes(b"outside")
            with self.assertRaises(relocation.RelocationInputError):
                relocation.file_tree_manifest(root)


class ExclusiveOutputTests(unittest.TestCase):
    def test_existing_regular_file_is_never_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "receipt.json"
            destination.write_bytes(b"original")
            with self.assertRaises(relocation.RelocationInputError):
                relocation.write_new_bytes(destination, b"replacement", label="test receipt")
            self.assertEqual(destination.read_bytes(), b"original")

    def test_existing_symlink_is_never_followed_or_replaced(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            target = root / "target.json"
            target.write_bytes(b"target remains unchanged")
            destination = root / "receipt.json"
            try:
                destination.symlink_to(target)
            except (NotImplementedError, OSError) as error:
                self.skipTest(f"symlink creation unavailable: {error}")
            with self.assertRaises(relocation.RelocationInputError):
                relocation.write_new_bytes(destination, b"replacement", label="test receipt")
            self.assertTrue(destination.is_symlink())
            self.assertEqual(target.read_bytes(), b"target remains unchanged")

    def test_concurrent_receipt_writers_have_exactly_one_winner(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "receipt.json"
            barrier = threading.Barrier(2)
            payloads = (b"writer one\n", b"writer two\n")

            def write(payload: bytes) -> bool:
                barrier.wait()
                try:
                    relocation.write_new_bytes(destination, payload, label="test receipt")
                except relocation.RelocationInputError:
                    return False
                return True

            with ThreadPoolExecutor(max_workers=2) as executor:
                results = list(executor.map(write, payloads))
            self.assertEqual(sum(results), 1)
            self.assertIn(destination.read_bytes(), payloads)


if __name__ == "__main__":
    unittest.main()
