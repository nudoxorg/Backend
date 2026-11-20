"""Checks exact build custody; no fixture grants an installed worker permit."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
import os
import sys
import subprocess
from unittest.mock import patch

import linux_release_package as package
import install_linux as installer
import record_embedding_helper_build as producer


class StyloWorkerReceiptTests(unittest.TestCase):
    def test_real_checkout_observation_ignores_foreign_git_redirection(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, foreign = root / "source", root / "foreign"
            source.mkdir()
            foreign.mkdir()
            for directory_path in (source, foreign):
                subprocess.run(["git", "-C", str(directory_path), "init", "-q"], check=True)
            (source / "Cargo.lock").write_text("fixture lock\n")
            subprocess.run(["git", "-C", str(source), "add", "Cargo.lock"], check=True)
            subprocess.run(["git", "-C", str(source), "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture"], check=True)
            expected = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
            redirect = {"GIT_DIR": str(foreign / ".git"), "GIT_WORK_TREE": str(foreign), "GIT_CONFIG_GLOBAL": str(root / "absent-config")}
            with patch.dict(os.environ, redirect):
                observed = producer.clean_source_snapshot(source)
                self.assertEqual(observed["commit"], expected)
                self.assertTrue(observed["clean"])
                self.assertEqual(os.environ["GIT_DIR"], redirect["GIT_DIR"])

    def test_installer_checks_optional_packaged_worker_without_granting_permit(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            (root / "bin").mkdir()
            evidence_dir = root / "share/nudox/stylo"
            evidence_dir.mkdir(parents=True)
            worker = root / "bin/backend-stylo-worker"
            worker.write_bytes(b"relocated worker")
            worker.chmod(0o700)
            source = {"commit": "a" * 40, "tree": "b" * 40, "cargo_lock_sha256": "c" * 64}
            receipt = {"schema": "nudox.stylo-worker-build-receipt.v2", "remap_schema": "nudox.stylo-worker-remap.v2",
                       "source": {**source, "clean_before": True, "clean_after": True},
                       "artifact": {"sha256": "d" * 64, "architecture": "x86_64"},
                       "argv": ["cargo", "build", "--release", "--locked", "-p", "backend-frontend-stylo", "--bin", "backend-stylo-worker"],
                       "exit": 0, "execution": {"exit": 0, "process_group_retired": True},
                       "toolchain": {"unchanged": True},
                       "environment": {"CARGO_BUILD_TARGET": package.TARGET, "CARGO_NET_OFFLINE": "true"}}
            evidence = evidence_dir / "build-receipt.json"
            evidence.write_text(json.dumps(receipt))
            record = {"path": "bin/backend-stylo-worker", "sha256": hashlib.sha256(worker.read_bytes()).hexdigest(),
                      "size_bytes": worker.stat().st_size, "source_sha256": "d" * 64,
                      "build_receipt": "share/nudox/stylo/build-receipt.json",
                      "build_receipt_sha256": hashlib.sha256(evidence.read_bytes()).hexdigest()}
            metadata = {"source": source, "stylo_worker": record}
            installer.verify_stylo_worker(root, metadata)
            metadata["original_build_manifest_sha256"] = "e" * 64
            metadata["executables"] = [{"name": "backend-locald", "original_sha256": "f" * 64}]
            (root / "lib").mkdir()
            dependency = root / "lib/libgcc_s.so.1"
            dependency.write_bytes(b"libgcc fixture")
            dep_sha = hashlib.sha256(dependency.read_bytes()).hexdigest()
            metadata["libraries"] = {"libgcc_s.so.1": {"packaged_sha256": dep_sha, "source_sha256": "5" * 64}}
            record["bound_owner"] = {"schema": "nudox.stylo-bound-owner-build-receipt.v4",
                "remap_schema": "nudox.stylo-owner-remap.v1", "c_remap_schema": "nudox.stylo-c-prefix-map.v1", "cli_build_manifest_sha256": "e" * 64,
                "closure_policy": "linux-libgcc-single-v1", "runpath_kind": "DT_RUNPATH",
                "receipt_sha256": "1" * 64, "owner_source_sha256": "f" * 64, "worker_anchor": "2" * 64,
                "source_loader_sha256": "6" * 64, "dependency_source_sha256": "5" * 64,
                "dependency_path": "lib/libgcc_s.so.1", "dependency_sha256": dep_sha, "dependency_bytes": dependency.stat().st_size,
                "relocation_tool_sha256": {"patchelf": "3" * 64, "readelf": "4" * 64}}
            installer.verify_stylo_worker(root, metadata)
            dependency.write_bytes(b"tampered dependency")
            with self.assertRaises(installer.InstallError): installer.verify_stylo_worker(root, metadata)
            dependency.write_bytes(b"libgcc fixture")
            record["bound_owner"]["owner_source_sha256"] = "9" * 64
            with self.assertRaises(installer.InstallError): installer.verify_stylo_worker(root, metadata)
            record["bound_owner"]["owner_source_sha256"] = "f" * 64
            with self.assertRaises(installer.InstallError): installer.verify_stylo_worker(root, {})
            worker.write_bytes(b"tampered worker")
            with self.assertRaises(installer.InstallError): installer.verify_stylo_worker(root, metadata)
            worker.write_bytes(b"relocated worker")
            evidence.write_text(json.dumps({**receipt, "exit": 1}))
            with self.assertRaises(installer.InstallError): installer.verify_stylo_worker(root, metadata)
            worker.unlink()
            with self.assertRaises(installer.InstallError): installer.verify_stylo_worker(root, metadata)
            installer.verify_stylo_worker(root, {})

    def test_requires_exact_clean_build_source_recipe_and_worker_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            worker = root / "backend-stylo-worker"
            worker.write_bytes(b"worker artifact fixture")
            private_paths = [str(root / item) for item in ("source", "cargo", "target", "build")]
            flags = "\x1f".join(f"--remap-path-prefix={value}=/nudox/{label}" for value, label in zip(private_paths, ("source", "cargo-home", "target", "build")))
            receipt = {
                "schema": "nudox.stylo-worker-build-receipt.v2", "remap_schema": "nudox.stylo-worker-remap.v2", "source_path": private_paths[0],
                "source": {"commit": "a" * 40, "tree": "b" * 40,
                           "cargo_lock_sha256": "c" * 64, "clean_before": True, "clean_after": True},
                "toolchain": {"unchanged": True, "rustc": {"path": "/fixture/rustc", "sha256": "e" * 64}, "cargo": {"path": "/fixture/cargo", "sha256": "e" * 64}}, "exit": 0,
                "build_helpers": {name: {"path": "/fixture/" + name, "sha256": "e" * 64} for name in ("cc", "make", "python3", "sh")},
                "execution": {"exit": 0, "process_group_retired": True},
                "environment": {"CARGO_BUILD_TARGET": package.TARGET, "CARGO_NET_OFFLINE": "true", "CARGO_BUILD_JOBS": "2", "CARGO_INCREMENTAL": "0", "CARGO_CACHE_AUTO_CLEAN_FREQUENCY": "never", "CARGO_TERM_COLOR": "never", "RUSTC_WRAPPER": "", "RUSTC_WORKSPACE_WRAPPER": "", "RUSTC": "/fixture/rustc", "CARGO_HOME": private_paths[1], "CARGO_TARGET_DIR": private_paths[2], "CARGO_BUILD_BUILD_DIR": private_paths[3], "HOME": private_paths[3], "TMPDIR": private_paths[3], "RUSTFLAGS": "", "CARGO_ENCODED_RUSTFLAGS": flags},
                "argv": ["cargo", "build", "--release", "--locked", "-p", "backend-frontend-stylo", "--bin", "backend-stylo-worker"],
                "artifact": {"bytes": worker.stat().st_size, "sha256": hashlib.sha256(worker.read_bytes()).hexdigest(), "architecture": "x86_64"},
            }
            path = root / "build-receipt.json"
            def admit(value):
                path.write_text(json.dumps(value))
                return package.admit_stylo_worker(worker, path, "a" * 40, "b" * 40, "c" * 64)
            record, digest = admit(receipt)
            self.assertEqual(record, {key: receipt["artifact"][key] for key in ("bytes", "sha256")})
            self.assertEqual(digest, hashlib.sha256(path.read_bytes()).hexdigest())
            for mutation in ("version", "remap", "exit", "source", "lock", "dirty", "toolchain", "argv", "digest", "size", "target", "process", "architecture", "helper", "compiler", "ambient"):
                changed = copy.deepcopy(receipt)
                if mutation == "version": changed["schema"] = "nudox.stylo-worker-build-receipt.v1"
                if mutation == "remap": changed["environment"]["CARGO_ENCODED_RUSTFLAGS"] = ""
                if mutation == "exit": changed["exit"] = False
                if mutation == "source": changed["source"]["tree"] = "d" * 40
                if mutation == "lock": changed["source"]["cargo_lock_sha256"] = "d" * 64
                if mutation == "dirty": changed["source"]["clean_after"] = False
                if mutation == "toolchain": changed["toolchain"]["unchanged"] = False
                if mutation == "argv": changed["argv"].remove("--locked")
                if mutation == "digest": changed["artifact"]["sha256"] = "d" * 64
                if mutation == "size": changed["artifact"]["bytes"] += 1
                if mutation == "target": changed["environment"]["CARGO_BUILD_TARGET"] = "other"
                if mutation == "process": changed["execution"]["process_group_retired"] = False
                if mutation == "architecture": changed["artifact"]["architecture"] = "aarch64"
                if mutation == "helper": del changed["build_helpers"]["cc"]
                if mutation == "compiler": changed["environment"]["RUSTC"] = "/other/rustc"
                if mutation == "ambient": changed["environment"]["RUSTC_BOOTSTRAP"] = "1"
                with self.subTest(mutation=mutation), self.assertRaises(ValueError):
                    admit(changed)
            worker.write_bytes(b"tampered worker fixture")
            with self.assertRaises(ValueError): admit(receipt)
            worker.write_bytes(b"prefix " + private_paths[1].encode() + b" suffix")
            changed = copy.deepcopy(receipt)
            changed["artifact"].update(bytes=worker.stat().st_size, sha256=hashlib.sha256(worker.read_bytes()).hexdigest())
            with self.assertRaisesRegex(ValueError, "private build path"): admit(changed)
            worker.unlink()
            with self.assertRaises((ValueError, OSError)): admit(receipt)


if __name__ == "__main__":
    unittest.main()
