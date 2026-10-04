"""Protocol oracles use synthetic local receipts; they never clone APFS or touch real builds."""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import plistlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import clone_closed_cargo_graph as clone


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def write(path: pathlib.Path, data: bytes, mode: int = 0o600) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(mode)


def private_dir(path: pathlib.Path) -> pathlib.Path:
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    path.chmod(0o700)
    return path


class SyntheticReceipt:
    def __init__(self, base: pathlib.Path, token: str, commit: str, tree: str,
                 shared_cache: pathlib.Path, identity: dict, *, with_graph_slot: bool):
        self.release = private_dir(base / f"{token}-release")
        self.source_root = private_dir(self.release / "source")
        self.receipt = private_dir(base / f"validation-{token}")
        self.cargo_home = private_dir(self.receipt / "cargo-home")
        self.target_dir = private_dir(self.receipt / "cargo-target")
        self.build_graph = private_dir(self.receipt / "cargo-build-graph")
        self.attempts = private_dir(self.receipt / "attempts")
        self.commit = commit
        self.tree = tree
        write(self.source_root / "Cargo.lock", b"synthetic-lock\n")
        write(self.source_root / "Cargo.toml", b"[workspace]\n")
        write(self.source_root / "src" / "lib.rs", f"// {token}\n".encode())
        release_receipt = private_dir(self.release / "receipt")
        manifest_bytes = json.dumps({"synthetic": token}, sort_keys=True).encode() + b"\n"
        write(release_receipt / "source-verification.json", manifest_bytes)
        self.manifest_sha = sha(manifest_bytes)
        self.lock_sha = sha(b"synthetic-lock\n")
        self.runner = self.receipt / f"run-cargo-{token}.sh"
        runner_bytes = (
            "#!/bin/sh\n"
            "export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=1 NUDOX_CARGO_BUILD_SLOTS=1\n"
            "export SCCACHE_CLIENT_SIDE=1 MACOSX_DEPLOYMENT_TARGET=26.0\n"
        ).encode()
        write(self.runner, runner_bytes, 0o700)
        self.wrapper_hash = sha(runner_bytes)
        self.layout = {
            "receipt": str(self.receipt),
            "source_release": str(self.release),
            "source_commit": commit,
            "source_tree": tree,
            "source_manifest_sha256": self.manifest_sha,
            "cargo_lock_sha256": self.lock_sha,
            "cargo_home": str(self.cargo_home),
            "target_dir": str(self.target_dir),
            "build_graph_dir": str(self.build_graph),
            "shared_cache": str(shared_cache),
            "status": "isolated_empty_build_roots_prepared",
            "old_source_target_or_build_graph_reused": False,
            "reviewed_wrapper_sha256": {self.runner.name: self.wrapper_hash},
        }
        write(self.receipt / "layout-result.json",
              json.dumps(self.layout, sort_keys=True).encode() + b"\n")
        self.slot = self.build_graph / ".nudox-cargo" / "slot-0"
        if with_graph_slot:
            private_dir(self.build_graph / ".nudox-cargo" / "leases")
            private_dir(self.slot)
            write(self.slot / ".nudox-worktree-root",
                  (str(self.source_root) + "\n").encode())
            private_dir(self.slot / "debug" / "deps")
            write(self.slot / "debug" / ".nudox-worktree-root", b"nested-stamp-name-is-data\n")
            write(self.slot / "debug" / "deps" / "libunchanged.rlib", b"synthetic-rlib\n", 0o644)
        self.provenance_run_id = ""
        self.attempt_id = ""
        self.expected_exit = 0
        self.identity = identity

    def close_source_attempt(self, attempt_id: str, run_id: str, expected_exit: int) -> None:
        self.attempt_id = attempt_id
        self.provenance_run_id = run_id
        self.expected_exit = expected_exit
        attempt = private_dir(self.attempts / attempt_id)
        owner = (
            f"stage={attempt_id}\n"
            "pid=99999991\n"
            "start=Mon Jan 1 00:00:00 2024\n"
            f"source={self.source_root}\n"
            f"source_commit={self.commit}\n"
            f"source_tree={self.tree}\n"
            f"cargo={self.identity['cargo_path']}\n"
            f"rustc={self.identity['rustc_path']}\n"
            f"rustc_wrapper_sha256={self.identity['rustc_wrapper_sha256']}\n"
            f"cargo_version={self.identity['cargo_version']}\n"
            "macos_deployment_target=26.0\n"
            "argv=test --locked --offline -j1 -p backend-library --lib -- --test-threads=2 \n"
        ).encode()
        log = b"Finished test profile [unoptimized + debuginfo] target(s) in 1.00s\n"
        graph_entries, _ = clone._tree_inventory(self.slot, "synthetic source graph", hash_contents=True)
        graph_metrics = clone._graph_inventory_metrics(graph_entries)
        write(attempt / "current-cargo-owner.txt", owner)
        write(attempt / "cargo.log", log)
        provenance = {
            "schema": 2,
            "run_id": run_id,
            "workspace_root": str(self.source_root),
            "git_head": None,
            "source_dirty_sha256": "a" * 64,
            "source_dirty_sha256_after": "a" * 64,
            "source_changed_during_build": False,
            "cargo_lock_sha256": self.lock_sha,
            "cargo_lock_sha256_after": self.lock_sha,
            "cargo_build_dir": str(self.slot),
            "cargo_target_dir": str(self.target_dir),
            "cargo_build_graph_inventory": {
                "schema": clone._GRAPH_INVENTORY_SCHEMA,
                "capture_complete": True,
                "capture_point": "after-cargo-exit-before-slot-release",
                "slot": str(self.slot),
                "inventory_sha256": clone._graph_output_inventory_sha256(graph_entries),
                **graph_metrics,
            },
            "build_environment": {
                "schema": clone._BUILD_ENVIRONMENT_SCHEMA,
                "capture_complete": True,
                "capture_point": "cargo-invocation",
                "values": self.identity["environment"],
                "sha256": self.identity["effective_environment_sha256"],
            },
            "features": self.identity["features"],
            "cargo_exit_status": expected_exit,
            "finished_at_utc": "2026-10-04T12:00:00+00:00",
            "toolchain": {
                "capture_complete": True,
                "changed_during_build": False,
                "executables_before": {
                    "cargo": {
                        "version": self.identity["cargo_version"],
                        "path": self.identity["cargo_path"],
                        "sha256": self.identity["cargo_executable_sha256"],
                    },
                    "rustc": {
                        "version": self.identity["rustc_version"],
                        "path": self.identity["rustc_path"],
                        "sha256": self.identity["rustc_executable_sha256"],
                    },
                },
            },
            "wrapper": {
                "runtime_sha256": self.identity["runtime_wrapper_sha256"],
                "source_sha256": self.identity["source_wrapper_sha256"],
                "rustc_sha256": self.identity["rustc_wrapper_sha256"],
            },
            "outputs": [],
        }
        write(self.target_dir / ".nudox-provenance" / f"{run_id}.json",
              json.dumps(provenance, sort_keys=True).encode() + b"\n")
        result = {
            "source_commit": self.commit,
            "source_tree": self.tree,
            "lock_sha256": self.lock_sha,
            "source_manifest_sha256": self.manifest_sha,
            "cargo_exit": str(expected_exit),
            "stage_exit": str(expected_exit),
            "source_verification_after_exit": "0",
            "rustc_wrapper_sha256_expected": self.identity["rustc_wrapper_sha256"],
            "rustc_wrapper_sha256_after": self.identity["rustc_wrapper_sha256"],
            "rustc_wrapper_verification_after_exit": "0",
            "cargo_completed_utc": "2026-10-04T12:00:00Z",
            "completed_utc": "2026-10-04T12:00:01Z",
            "owner_record_sha256": sha(owner),
            "log_sha256": sha(log),
        }
        write(attempt / "result.txt",
              "".join(f"{key}={value}\n" for key, value in result.items()).encode())


class CloneProtocolTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(
            prefix="nudox-closed-graph-synthetic-", dir="/private/tmp"
        )
        self.base = pathlib.Path(self.temp.name)
        self.shared_cache = private_dir(self.base / "shared-cargo-cache")
        self.identity = {
            "cargo_version": "cargo 1.97.1 (synthetic)",
            "cargo_path": "/nix/store/synthetic-cargo/bin/cargo",
            "cargo_executable_sha256": "1" * 64,
            "rustc_version": (
                "rustc 1.97.1 (synthetic)\n"
                "host: aarch64-apple-darwin\n"
                "release: 1.97.1\n"
            ),
            "rustc_path": "/nix/store/synthetic-rustc/bin/rustc",
            "rustc_executable_sha256": "2" * 64,
            "rustc_wrapper_sha256": "3" * 64,
            "runtime_wrapper_sha256": "4" * 64,
            "source_wrapper_sha256": "5" * 64,
            "command": [
                "test", "--locked", "--offline", "-j1", "-p",
                "backend-library", "--lib", "--", "--test-threads=2",
            ],
            "profile": "test",
            "target_triple": "aarch64-apple-darwin",
            "features": {
                "features": [],
                "all_features": False,
                "no_default_features": False,
                "targets": [],
            },
            "environment": {
                "CARGO_INCREMENTAL": "0",
                "CARGO_BUILD_JOBS": "1",
                "NUDOX_CARGO_BUILD_SLOTS": "1",
                "MACOSX_DEPLOYMENT_TARGET": "26.0",
                "RUSTFLAGS": None,
                "CARGO_ENCODED_RUSTFLAGS": None,
                "CARGO_BUILD_TARGET": None,
                "RUSTDOCFLAGS": None,
            },
            "shared_cache": str(self.shared_cache),
            "same_input_sha256": {},
            "lock_sha256": "",
        }
        self.identity["effective_environment_sha256"] = clone._effective_environment_sha256(
            self.identity["environment"]
        )
        self.source = SyntheticReceipt(
            self.base, "source", "a" * 40, "b" * 40, self.shared_cache, self.identity,
            with_graph_slot=True,
        )
        self.destination = SyntheticReceipt(
            self.base, "destination", "c" * 40, "d" * 40, self.shared_cache, self.identity,
            with_graph_slot=False,
        )
        self.source.close_source_attempt("attempt-closed", "1700000000-1234", 101)
        self.identity["lock_sha256"] = self.source.lock_sha
        self.identity["same_input_sha256"] = {
            "Cargo.toml": sha(b"[workspace]\n"),
            "Cargo.lock": self.source.lock_sha,
        }
        for side in (self.source, self.destination):
            side.layout["cargo_lock_sha256"] = self.source.lock_sha
            write(side.receipt / "layout-result.json",
                  json.dumps(side.layout, sort_keys=True).encode() + b"\n")
            side.identity = self.identity
        self.contract = {
            "schema": 1,
            "source": {
                "receipt": str(self.source.receipt),
                "attempt_id": "attempt-closed",
                "provenance_run_id": "1700000000-1234",
                "source_root": str(self.source.source_root),
                "commit": self.source.commit,
                "tree": self.source.tree,
                "source_manifest_sha256": self.source.manifest_sha,
                "lock_sha256": self.source.lock_sha,
                "expected_cargo_exit": 101,
                "reuse_identity": self.identity,
            },
            "destination": {
                "receipt": str(self.destination.receipt),
                "source_root": str(self.destination.source_root),
                "commit": self.destination.commit,
                "tree": self.destination.tree,
                "source_manifest_sha256": self.destination.manifest_sha,
                "lock_sha256": self.source.lock_sha,
                "reuse_identity": self.identity,
            },
            "reuse_identity": self.identity,
        }
        self._write_layout(self.destination.receipt)
        self._write_layout(self.source.receipt)

    def tearDown(self) -> None:
        self.temp.cleanup()

    def _write_layout(self, receipt: pathlib.Path) -> None:
        layout_path = receipt / "layout-result.json"
        layout = json.loads(layout_path.read_text())
        layout["cargo_lock_sha256"] = self.source.lock_sha
        write(layout_path, json.dumps(layout, sort_keys=True).encode() + b"\n")

    def run_clone(self, **kwargs):
        return clone.clone_closed_graph(
            self.contract,
            start_lookup=kwargs.pop("start_lookup", lambda pid: None),
            **kwargs,
        )

    def test_dry_run_is_read_only_and_reports_no_clone_claim(self) -> None:
        report = self.run_clone(execute=False)
        self.assertEqual(report["status"], "validated_plan")
        self.assertFalse((self.destination.build_graph / ".nudox-cargo").exists())
        self.assertTrue(report["source_graph_outputs_hash_attested_by_original_receipt"])
        self.assertTrue(report["source_effective_build_environment_attested_by_original_receipt"])
        self.assertEqual(report["reuse_attestation_issues"], [])
        self.assertEqual(report["destination_cargo_run"], "not_run")

    def test_unattested_closed_run_is_reported_and_refused_before_clone(self) -> None:
        provenance_path = (
            self.source.target_dir / ".nudox-provenance" / f"{self.source.provenance_run_id}.json"
        )
        provenance = json.loads(provenance_path.read_text())
        provenance.pop("cargo_build_graph_inventory")
        provenance.pop("build_environment")
        write(provenance_path, json.dumps(provenance, sort_keys=True).encode() + b"\n")

        report = self.run_clone(execute=False)
        self.assertEqual(report["status"], "validated_plan_unattested")
        self.assertFalse(report["source_graph_outputs_hash_attested_by_original_receipt"])
        self.assertFalse(report["source_effective_build_environment_attested_by_original_receipt"])
        self.assertEqual(len(report["reuse_attestation_issues"]), 2)

        with self.assertRaisesRegex(clone.SafetyError, "without matching closed-run graph and environment provenance"):
            self.run_clone(
                execute=True,
                clone_one=lambda src, dst: self.fail("clone callback must not run"),
                apfs_check=lambda src, dst: self.fail("filesystem query must not run"),
            )
        self.assertFalse((self.destination.build_graph / ".nudox-cargo").exists())

    def test_synthetic_copy_protocol_rebinds_stamp_and_preserves_source(self) -> None:
        def fake_clonefile(src: pathlib.Path, dst: pathlib.Path) -> None:
            shutil.copy2(src, dst)

        with mock.patch.object(clone, "_process_start_token", return_value="Synthetic Start"):
            report = self.run_clone(
                execute=True,
                clone_one=fake_clonefile,
                apfs_check=lambda source, destination: None,
            )
        destination_slot = self.destination.build_graph / ".nudox-cargo" / "slot-0"
        self.assertEqual(report["status"], "synthetic_clone_protocol_exercised")
        self.assertEqual(report["clone_method"], "injected test callback")
        self.assertEqual((destination_slot / clone.STAMP).read_text().strip(),
                         str(self.destination.source_root))
        self.assertEqual((self.source.slot / clone.STAMP).read_text().strip(),
                         str(self.source.source_root))
        self.assertEqual((destination_slot / "debug" / clone.STAMP).read_bytes(),
                         b"nested-stamp-name-is-data\n")
        self.assertTrue(report["source_inventory_unchanged"])
        self.assertEqual(report["source_inventory_sha256_before"],
                         report["source_inventory_sha256_after"])
        self.assertEqual(report["cloned_regular_files"], 2)
        self.assertEqual(list((self.destination.build_graph / ".nudox-cargo" / "leases").iterdir()), [])

    def test_active_owner_refuses(self) -> None:
        with self.assertRaisesRegex(clone.SafetyError, "still active"):
            self.run_clone(execute=False,
                           start_lookup=lambda pid: "Mon Jan 1 00:00:00 2024")

    def test_destination_active_owner_refuses(self) -> None:
        attempt = private_dir(self.destination.attempts / "attempt-active")
        write(attempt / "current-cargo-owner.txt",
              b"pid=99999991\nstart=Mon Jan 1 00:00:00 2024\n")
        with self.assertRaisesRegex(clone.SafetyError, "still active"):
            self.run_clone(execute=False,
                           start_lookup=lambda pid: "Mon Jan 1 00:00:00 2024")

    def test_nonempty_source_lease_refuses(self) -> None:
        write(self.source.build_graph / ".nudox-cargo" / "leases" / "slot-1.lock" / "pid", b"999\n")
        with self.assertRaisesRegex(clone.SafetyError, "lease is not empty"):
            self.run_clone(execute=False)

    def test_nonempty_destination_lease_refuses(self) -> None:
        write(self.destination.build_graph / ".nudox-cargo" / "leases" / "slot-1.lock" / "pid",
              b"999\n")
        with self.assertRaisesRegex(clone.SafetyError, "destination has a role-graph lease"):
            self.run_clone(execute=False)

    def test_source_symlink_refuses(self) -> None:
        (self.source.slot / "bad-link").symlink_to(self.source.slot / "debug" / "deps" / "libunchanged.rlib")
        with self.assertRaisesRegex(clone.SafetyError, "symlink"):
            self.run_clone(execute=False)

    def test_contract_symlinked_source_root_refuses(self) -> None:
        alias = self.base / "source-root-alias"
        alias.symlink_to(self.source.source_root, target_is_directory=True)
        bad = json.loads(json.dumps(self.contract))
        bad["source"]["source_root"] = str(alias)
        with self.assertRaisesRegex(clone.SafetyError, "symlink component"):
            clone.clone_closed_graph(bad, execute=False, start_lookup=lambda pid: None)

    def test_source_hardlink_refuses(self) -> None:
        original = self.source.slot / "debug" / "deps" / "libunchanged.rlib"
        os.link(original, self.base / "outside-hardlink.rlib")
        with self.assertRaisesRegex(clone.SafetyError, "hard-linked"):
            self.run_clone(execute=False)

    def test_lock_or_toolchain_mismatch_refuses(self) -> None:
        bad = json.loads(json.dumps(self.contract))
        bad["destination"]["lock_sha256"] = "f" * 64
        with self.assertRaisesRegex(clone.SafetyError, "lock hashes"):
            clone.clone_closed_graph(bad, execute=False, start_lookup=lambda pid: None)
        bad = json.loads(json.dumps(self.contract))
        bad["destination"]["reuse_identity"]["rustc_executable_sha256"] = "f" * 64
        with self.assertRaisesRegex(clone.SafetyError, "exactly equal"):
            clone.clone_closed_graph(bad, execute=False, start_lookup=lambda pid: None)

    def test_nonempty_destination_refuses(self) -> None:
        write(self.destination.target_dir / "unexpected-output", b"do not overwrite")
        with self.assertRaisesRegex(clone.SafetyError, "target directory is not empty"):
            self.run_clone(execute=False)

    def test_existing_destination_slot_refuses(self) -> None:
        private_dir(self.destination.build_graph / ".nudox-cargo" / "slot-0")
        with self.assertRaisesRegex(clone.SafetyError, "already exists"):
            self.run_clone(execute=False)

    def test_dangling_destination_namespace_symlink_refuses(self) -> None:
        lane = self.destination.build_graph / ".nudox-cargo"
        lane.symlink_to(self.base / "missing-destination-lane", target_is_directory=True)
        with self.assertRaisesRegex(clone.SafetyError, "symlink"):
            self.run_clone(execute=False)

    def test_non_apfs_or_cross_volume_refuses_before_destination_mutation(self) -> None:
        with self.assertRaisesRegex(clone.SafetyError, "synthetic filesystem refusal"):
            self.run_clone(
                execute=True,
                clone_one=lambda src, dst: self.fail("clonefile must not run"),
                apfs_check=lambda source, destination: (_ for _ in ()).throw(
                    clone.SafetyError("synthetic filesystem refusal")
                ),
            )
        self.assertFalse((self.destination.build_graph / ".nudox-cargo").exists())

    def test_source_mutation_during_clone_removes_candidate(self) -> None:
        changed = False

        def mutating_clone(src: pathlib.Path, dst: pathlib.Path) -> None:
            nonlocal changed
            shutil.copy2(src, dst)
            if src.name == "libunchanged.rlib" and not changed:
                changed = True
                (self.source.slot / "debug" / "deps" / "libunchanged.rlib").write_bytes(b"changed")

        with mock.patch.object(clone, "_process_start_token", return_value="Synthetic Start"):
            with self.assertRaisesRegex(clone.SafetyError, "inventory changed"):
                self.run_clone(execute=True, clone_one=mutating_clone,
                               apfs_check=lambda source, destination: None)
        self.assertFalse((self.destination.build_graph / ".nudox-cargo" / "slot-0").exists())
        leases = self.destination.build_graph / ".nudox-cargo" / "leases"
        self.assertEqual(list(leases.iterdir()), [])

    def test_partial_clone_failure_cleans_stage_and_leases(self) -> None:
        def fail_clone(src: pathlib.Path, dst: pathlib.Path) -> None:
            raise clone.SafetyError("synthetic clone failure")

        with mock.patch.object(clone, "_process_start_token", return_value="Synthetic Start"):
            with self.assertRaisesRegex(clone.SafetyError, "synthetic clone failure"):
                self.run_clone(execute=True, clone_one=fail_clone,
                               apfs_check=lambda source, destination: None)
        namespace = self.destination.build_graph / ".nudox-cargo"
        self.assertFalse((namespace / "slot-0").exists())
        self.assertFalse(any(path.name.startswith(".slot-0-clone.tmp-")
                             for path in namespace.iterdir()))
        self.assertEqual(list((namespace / "leases").iterdir()), [])

    def test_lease_release_failure_removes_installed_candidate(self) -> None:
        def fake_clonefile(src: pathlib.Path, dst: pathlib.Path) -> None:
            shutil.copy2(src, dst)

        release = clone._release_role_graph_leases

        def release_then_fail(locks: list[pathlib.Path]) -> None:
            release(locks)
            raise clone.SafetyError("synthetic lease release failure")

        with mock.patch.object(clone, "_process_start_token", return_value="Synthetic Start"), \
                mock.patch.object(clone, "_release_role_graph_leases", side_effect=release_then_fail):
            with self.assertRaisesRegex(clone.SafetyError, "synthetic lease release failure"):
                self.run_clone(execute=True, clone_one=fake_clonefile,
                               apfs_check=lambda source, destination: None)
        namespace = self.destination.build_graph / ".nudox-cargo"
        self.assertFalse((namespace / "slot-0").exists())
        self.assertFalse(any(path.name.startswith(".slot-0-clone.tmp-")
                             for path in namespace.iterdir()))
        self.assertEqual(list((namespace / "leases").iterdir()), [])

    def test_clone_hardlink_output_refuses_and_cleans_candidate(self) -> None:
        source_before = clone._tree_inventory(self.source.slot, "test source before", hash_contents=True)

        def linked_copy(src: pathlib.Path, dst: pathlib.Path) -> None:
            shutil.copy2(src, dst)
            os.link(dst, dst.with_name(dst.name + ".alias"))

        with mock.patch.object(clone, "_process_start_token", return_value="Synthetic Start"):
            with self.assertRaisesRegex(clone.SafetyError, "hard-linked file"):
                self.run_clone(execute=True, clone_one=linked_copy,
                               apfs_check=lambda source, destination: None)
        source_after = clone._tree_inventory(self.source.slot, "test source after", hash_contents=True)
        self.assertEqual(source_before, source_after)
        namespace = self.destination.build_graph / ".nudox-cargo"
        self.assertFalse((namespace / "slot-0").exists())
        self.assertFalse(any(path.name.startswith(".slot-0-clone.tmp-")
                             for path in namespace.iterdir()))

    def test_destination_runner_target_mismatch_refuses(self) -> None:
        runner = next(self.destination.receipt.glob("run-cargo*.sh"))
        content = runner.read_text().replace("CARGO_INCREMENTAL=0", "CARGO_INCREMENTAL=1")
        write(runner, content.encode(), 0o700)
        layout = self.destination.layout
        layout["reviewed_wrapper_sha256"][runner.name] = sha(content.encode())
        write(self.destination.receipt / "layout-result.json",
              json.dumps(layout, sort_keys=True).encode() + b"\n")
        with self.assertRaisesRegex(clone.SafetyError, "CARGO_INCREMENTAL differs"):
            self.run_clone(execute=False)

    def test_run_cargo_scripts_must_match_exactly(self) -> None:
        runner = next(self.destination.receipt.glob("run-cargo*.sh"))
        content = runner.read_text() + "# extra destination-only wrapper behavior\n"
        write(runner, content.encode(), 0o700)
        layout = self.destination.layout
        layout["reviewed_wrapper_sha256"][runner.name] = sha(content.encode())
        write(self.destination.receipt / "layout-result.json",
              json.dumps(layout, sort_keys=True).encode() + b"\n")
        with self.assertRaisesRegex(clone.SafetyError, "run-cargo scripts differ"):
            self.run_clone(execute=False)

    def test_runner_target_environment_must_match_contract(self) -> None:
        runner = next(self.destination.receipt.glob("run-cargo*.sh"))
        content = runner.read_text() + "export CARGO_BUILD_TARGET=wrong-target\n"
        write(runner, content.encode(), 0o700)
        layout = self.destination.layout
        layout["reviewed_wrapper_sha256"][runner.name] = sha(content.encode())
        write(self.destination.receipt / "layout-result.json",
              json.dumps(layout, sort_keys=True).encode() + b"\n")
        with self.assertRaisesRegex(clone.SafetyError, "CARGO_BUILD_TARGET differs"):
            self.run_clone(execute=False)


class CargoArgumentTests(unittest.TestCase):
    def test_profile_parser_observes_release_and_stops_at_harness_separator(self) -> None:
        self.assertEqual(clone._profile_from_command(["build", "--release"]), "release")
        self.assertEqual(clone._profile_from_command(["test", "--", "--release"]), "test")
        self.assertEqual(clone._target_from_command(
            ["test", "--", "--target", "harness-only"],
            "rustc 1.97.1\nhost: aarch64-apple-darwin\n",
        ), "aarch64-apple-darwin")

    def test_malformed_profile_target_refuses(self) -> None:
        with self.assertRaisesRegex(clone.SafetyError, "missing its value"):
            clone._target_from_command(["build", "--target"], "rustc 1.97.1\nhost: x\n")
        with self.assertRaisesRegex(clone.SafetyError, "combines --profile"):
            clone._profile_from_command(["build", "--profile=release", "--release"])

    def test_native_df_and_diskutil_plist_formats_resolve_apfs_device(self) -> None:
        df_output = (
            "Filesystem 1024-blocks Used Available Capacity Mounted on\n"
            "/dev/disk3s5 1950000000 123456 1949876544 1% /System/Volumes/Data\n"
        )
        device = clone._parse_df_device(df_output)
        native_info_shape = plistlib.dumps({
            "DeviceIdentifier": "disk3s5",
            "FilesystemType": "apfs",
            "FilesystemName": "APFS",
            "FilesystemUserVisibleName": "APFS",
            "MountPoint": "/System/Volumes/Data",
        })
        self.assertEqual(device, "/dev/disk3s5")
        self.assertEqual(clone._parse_diskutil_info_plist(native_info_shape, device), {
            "filesystem_type": "apfs",
            "device_identifier": "disk3s5",
        })

    def test_native_filesystem_query_rejects_mismatched_device_identity(self) -> None:
        plist = plistlib.dumps({"DeviceIdentifier": "disk9s1", "FilesystemType": "apfs"})
        with self.assertRaisesRegex(clone.SafetyError, "different DeviceIdentifier"):
            clone._parse_diskutil_info_plist(plist, "/dev/disk3s5")


if __name__ == "__main__":
    unittest.main(verbosity=2)
