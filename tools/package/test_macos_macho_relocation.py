#!/usr/bin/env python3
"""Data-contract tests for admitted macOS runtime and Mach-O relocation inputs.

These tests use ordinary files and do not establish native Mach-O closure,
codesign, app-launch, or runtime behavior.
"""

from __future__ import annotations

import importlib.util
import base64
import hashlib
import json
import sys
import tarfile
import tempfile
import threading
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import Any
from unittest.mock import patch


PACKAGE_DIR = Path(__file__).resolve().parent


def load_module(filename: str, name: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, PACKAGE_DIR / filename)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {filename}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


relocation = load_module("macho_relocation.py", "macho_relocation")
collector = load_module("collect-macos-macho-relocation.py", "collect_macos_macho_relocation")


class PackageNoticeAssemblyTests(unittest.TestCase):
    def test_admitted_package_notice_is_decoded_and_copied_exactly(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / "package"
            package.mkdir()
            staging = root / "Nudox.app"
            content = b"Authentic package copyright and license\n"
            digest = hashlib.sha256(content).hexdigest()
            plan = {"images": [], "loads": [], "packages": {"example": {
                "root_tree_sha256": relocation.file_tree_sha256(relocation.file_tree_manifest(package)),
                "notices": [{"name": "LICENSE.txt", "sha256": digest,
                             "content_base64": base64.b64encode(content).decode("ascii")}],
            }}}
            result = collector.bundle.apply_macho_relocation(staging, plan, {"example": package})
            relative = "Contents/Resources/Licenses/Third Party/example/LICENSE.txt"
            self.assertEqual((staging / relative).read_bytes(), content)
            self.assertEqual(result["notices"], [{"package_id": "example", "name": "LICENSE.txt",
                                                "sha256": digest, "path": relative}])

    def test_changed_notice_bytes_are_refused_before_copy(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            package = root / "package"
            package.mkdir()
            staging = root / "Nudox.app"
            plan = {"images": [], "loads": [], "packages": {"example": {
                "root_tree_sha256": relocation.file_tree_sha256(relocation.file_tree_manifest(package)),
                "notices": [{"name": "LICENSE.txt", "sha256": hashlib.sha256(b"admitted").hexdigest(),
                             "content_base64": base64.b64encode(b"changed").decode("ascii")}],
            }}}
            with self.assertRaisesRegex(collector.bundle.PackageError, "notice bytes changed"):
                collector.bundle.apply_macho_relocation(staging, plan, {"example": package})
            self.assertFalse(staging.exists())


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


class CollectorRpathTests(unittest.TestCase):
    def _collect(self, root: Path, files: dict[str, dict[str, Any]], owners: tuple[str, ...]) -> dict[str, Any]:
        image_rows: dict[str, dict[str, Any]] = {}
        path_to_ref: dict[Path, str] = {}
        dependencies: dict[Path, list[str]] = {}
        rpaths: dict[Path, list[str]] = {}
        for relative, evidence in files.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(f"synthetic Mach-O input: {relative}\n".encode())
            canonical = path.resolve(strict=True)
            ref = f"origin:application:{relative}"
            bundle_path = f"Contents/MacOS/{relative}"
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            image_rows[ref] = {
                "ref": ref,
                "kind": "origin",
                "source_root": "application",
                "source_relative_path": relative,
                "bundle_path": bundle_path,
                "sha256": digest,
                "architectures": ["arm64"],
                "minimum_macos": "14.0",
                "signature": "unsigned",
                "dylib_id": None,
                "rpaths": list(evidence.get("rpaths", [])),
                "remove_rpaths": [],
                "origin_id": "application",
            }
            path_to_ref[canonical] = ref
            dependencies[canonical] = list(evidence.get("dependencies", []))
            rpaths[canonical] = list(evidence.get("rpaths", []))

        input_root = collector.InputRoot(
            "application", root, "origin", "application", "Contents/MacOS"
        )
        owner_paths = {
            f"Contents/MacOS/{owner}": root / owner for owner in owners
        }
        process_roots = {
            owner: f"Contents/MacOS/{owner}" for owner in owners
        }
        origins = {
            "application": {
                "receipt_kind": "application",
                "receipt_sha256": "1" * 64,
                "root_tree_sha256": "2" * 64,
                "bundle_prefix": "Contents/MacOS",
                "process_roots": process_roots,
                "image_hashes": {},
            }
        }

        def fake_run(command: list[str], **_kwargs: Any) -> str:
            if command[:2] != ["otool", "-L"]:
                raise AssertionError(f"unexpected synthetic inspector command: {command!r}")
            path = Path(command[2]).resolve(strict=True)
            rows = "".join(
                f"\t{name} (compatibility version 1.0.0, current version 1.0.0)\n"
                for name in dependencies[path]
            )
            return f"{path}:\n{rows}"

        with (
            patch.object(collector, "macho_images", return_value=(image_rows, path_to_ref)),
            patch.object(collector.bundle, "run", side_effect=fake_run),
            patch.object(
                collector.bundle,
                "inspect_load_metadata",
                side_effect=lambda path: (rpaths[Path(path).resolve(strict=True)], None),
            ),
        ):
            return collector.collect_plan(
                {
                    "source": {"git_revision": "3" * 40, "git_tree": "4" * 40},
                    "target": {"triple": "aarch64-apple-darwin", "architecture": "arm64"},
                },
                origins,
                {"application": input_root},
                owner_paths,
                {},
                "arm64",
            )

    def test_absolute_rpath_is_matched_against_typed_input_root(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            runpath = root / "vendor"
            runpath.mkdir(parents=True)
            plan = self._collect(
                root,
                {"backend-desktop": {"rpaths": [str(runpath)]}},
                ("backend-desktop",),
            )
            self.assertEqual(plan["images"][0]["remove_rpaths"], [str(runpath)])

    def test_parent_loader_rpath_is_inherited_by_transitive_dependency(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            shared_dir = root / "parent-rpath"
            plan = self._collect(
                root,
                {
                    "backend-desktop": {"dependencies": [str(root / "libA.dylib")]},
                    "libA.dylib": {
                        "dependencies": [str(root / "libB.dylib")],
                        "rpaths": [str(shared_dir)],
                    },
                    "libB.dylib": {"dependencies": ["@rpath/libShared.dylib"]},
                    "parent-rpath/libShared.dylib": {},
                },
                ("backend-desktop",),
            )
            edges = {
                (row["image_ref"], row["install_name"]): row.get("target_ref")
                for row in plan["loads"]
            }
            self.assertEqual(
                edges[("origin:application:libB.dylib", "@rpath/libShared.dylib")],
                "origin:application:parent-rpath/libShared.dylib",
            )

    def test_current_image_rpath_precedes_inherited_parent_rpath(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            plan = self._collect(
                root,
                {
                    "backend-desktop": {"dependencies": [str(root / "libA.dylib")]},
                    "libA.dylib": {
                        "dependencies": [str(root / "libB.dylib")],
                        "rpaths": [str(root / "parent-rpath")],
                    },
                    "libB.dylib": {
                        "dependencies": ["@rpath/libShared.dylib"],
                        "rpaths": [str(root / "own-rpath")],
                    },
                    "own-rpath/libShared.dylib": {},
                    "parent-rpath/libShared.dylib": {},
                },
                ("backend-desktop",),
            )
            edges = {
                (row["image_ref"], row["install_name"]): row.get("target_ref")
                for row in plan["loads"]
            }
            self.assertEqual(
                edges[("origin:application:libB.dylib", "@rpath/libShared.dylib")],
                "origin:application:own-rpath/libShared.dylib",
            )

    def test_shared_image_with_conflicting_process_runpaths_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "app"
            with self.assertRaisesRegex(
                collector.RelocationInputError,
                "same image/load resolves to different targets in process contexts",
            ):
                self._collect(
                    root,
                    {
                        "backend-a": {
                            "dependencies": [str(root / "libSharedLoader.dylib")],
                            "rpaths": [str(root / "runpath-a")],
                        },
                        "backend-b": {
                            "dependencies": [str(root / "libSharedLoader.dylib")],
                            "rpaths": [str(root / "runpath-b")],
                        },
                        "libSharedLoader.dylib": {
                            "dependencies": ["@rpath/libTarget.dylib"]
                        },
                        "runpath-a/libTarget.dylib": {},
                        "runpath-b/libTarget.dylib": {},
                    },
                    ("backend-a", "backend-b"),
                )


class BundleToolPrerequisiteTests(unittest.TestCase):
    def test_relocator_requires_install_name_tool(self) -> None:
        with patch.object(collector.bundle.platform, "system", return_value="Darwin"), patch.object(
            collector.bundle.shutil,
            "which",
            side_effect=lambda name: None if name == "install_name_tool" else f"/usr/bin/{name}",
        ):
            with self.assertRaisesRegex(
                collector.bundle.PackageError, "install_name_tool"
            ):
                collector.bundle.macos_tools()


if __name__ == "__main__":
    unittest.main()
